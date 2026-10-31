//! The service: single-flight, the synthesis gate, the scheduler, eviction,
//! the circuit breaker, the boot sweep and the orphan reaper.
//!
//! Everything a client could once get wrong lives here, where a client cannot
//! undo it: one synthesis per utterance, ever, is a property of the SERVICE.
//!
//! What is NOT here any more is the substrate daemon's self-update protocol
//! (`update.rs`: watch its own binary, arm, hold a quiet window, `execv`, keep a
//! last-good copy). Cauldron owns the version, the restart and the rollback
//! now. What survived of it is the one thing only the daemon can know — when
//! it is quiet — as [`TtsManager::drain`], which closes the scheduler and waits
//! for in-flight syntheses before the process exits on its stop signal.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Notify, OwnedSemaphorePermit, Semaphore};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::hash::{self, hash_for, is_hash};
use crate::keys::{GlobalConfig, KeyConfig, KeysFile, Mode};
use crate::protocol::Push;
use crate::queue::{Job, KeyOrder, Lane, Queue};
use crate::store::{Counter, Store};
use crate::synth::{self, Cancel, CancelHandle, ModuleConfig, SynthModule};

/// How many times one job goes back on the queue after its backend child was
/// killed from outside. One: a signal says nothing about the text, so one
/// interruption earns a retry, but whatever is sending signals may still be at
/// it and an unbounded re-queue would spin on the machine's problem.
pub const INTERRUPTED_REQUEUES: u32 = 1;

/// A key resolved against the module registry, once, at load.
#[derive(Clone)]
pub struct ResolvedKey {
    pub cfg: KeyConfig,
    pub module: Arc<dyn SynthModule>,
    pub module_config: ModuleConfig,
    pub fingerprint: String,
    pub ext: &'static str,
}

/// What a waiter is waiting for.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderState {
    Queued,
    Synthesizing,
    Warm { rel_path: String, bytes: u64, synth_ms: u64 },
    Failed { code: String, reason: String },
}

impl RenderState {
    pub fn name(&self) -> &'static str {
        match self {
            RenderState::Queued => "queued",
            RenderState::Synthesizing => "synthesizing",
            RenderState::Warm { .. } => "warm",
            RenderState::Failed { .. } => "failed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, RenderState::Warm { .. } | RenderState::Failed { .. })
    }

    fn failed(code: &str, reason: impl Into<String>) -> Self {
        RenderState::Failed { code: code.into(), reason: reason.into() }
    }
}

/// What a PUT decided, said out loud. "It decides on the fly, should we warm
/// this one up now or is it already warm or is it already being warmed up?"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutAction {
    /// Already warm: nothing synthesized, the text discarded.
    Warm,
    /// Already being warmed: attached to that flight, the text discarded.
    Joined,
    /// Cold: this PUT's text is the one that will be spoken.
    Enqueued,
}

impl PutAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            PutAction::Warm => "warm",
            PutAction::Joined => "joined",
            PutAction::Enqueued => "enqueued",
        }
    }

    pub fn text_used(&self) -> bool {
        *self == PutAction::Enqueued
    }
}

/// One item of a `tts.register` batch. Priority rides on the ITEM — "not on
/// the queue itself, but on the individual text-to-speech items" — so one call
/// can carry a whole look-ahead window, each message on its own rung. `None`
/// takes the cache's default.
#[derive(Debug, Clone)]
pub struct RegisterItem {
    pub text: String,
    pub priority: Option<i64>,
}

impl RegisterItem {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), priority: None }
    }

    pub fn with_priority(text: impl Into<String>, priority: i64) -> Self {
        Self { text: text.into(), priority: Some(priority) }
    }
}

impl From<String> for RegisterItem {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for RegisterItem {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// Per-module circuit breaker. After `breaker_threshold` consecutive failures
/// the module is degraded: requests fail at once with `backend_unavailable`
/// instead of queueing behind a wedged backend, and one probe is let through
/// every `breaker_probe_secs`. That turns a 70-minute silent outage into
/// something a client learns in under a second.
#[derive(Debug, Clone, Default)]
struct Breaker {
    consecutive: u32,
    degraded_since: Option<Instant>,
    next_probe: Option<Instant>,
    reason: Option<String>,
}

/// A live single-flight CLAIM: one per hash that is queued or synthesizing.
///
/// Created before its job is pushed and destroyed only by
/// [`TtsManager::finish`]. Any path that takes a job off the queue without
/// finishing it strands the claim, and every later request for that text
/// joins a synthesis that does not exist — eviction under pressure once did
/// exactly that. One map, one lock, so there is nothing to invert.
struct Flight {
    tx: Arc<watch::Sender<RenderState>>,
    rx: watch::Receiver<RenderState>,
    cancel: CancelHandle,
    cancel_rx: Cancel,
}

/// The queue and the gate at the instant a `tts.request` missed: the two
/// facts a client cannot recover afterwards, and the ones that tell "cold"
/// from "stuck behind two hundred things" from "every permit spent".
#[derive(Debug, Clone, Copy, Default)]
struct MissSnapshot {
    queued_behind: u64,
    queued_behind_key: u64,
    gate_busy: u64,
    gate_max: u64,
}

impl MissSnapshot {
    fn merge_into(&self, v: &mut Value) {
        v["queued_behind"] = json!(self.queued_behind);
        v["queued_behind_key"] = json!(self.queued_behind_key);
        v["gate_busy"] = json!(self.gate_busy);
        v["gate_max"] = json!(self.gate_max);
    }
}

pub struct Inner {
    pub store: Store,
    pub config: Config,
    pub data_dir: PathBuf,
    pub boot_id: String,
    keys: RwLock<HashMap<String, ResolvedKey>>,
    globals: RwLock<GlobalConfig>,
    /// One permit per concurrent synthesis. NOTHING synthesizes without one,
    /// the interactive lane included.
    sem: Arc<Semaphore>,
    /// What `sem` was last sized to; a tokio semaphore cannot be asked.
    sem_capacity: AtomicU64,
    inflight: Mutex<HashMap<String, Flight>>,
    queue: Mutex<Queue>,
    notify: Notify,
    events: broadcast::Sender<Push>,
    breakers: Mutex<HashMap<String, Breaker>>,
    in_flight: AtomicU64,
    /// Set by [`TtsManager::drain`]: the scheduler starts nothing new.
    draining: AtomicBool,
    started: std::sync::OnceLock<()>,
}

#[derive(Clone)]
pub struct TtsManager(Arc<Inner>);

impl std::ops::Deref for TtsManager {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl TtsManager {
    pub fn new(store: Store, config: Config, data_dir: PathBuf, file: KeysFile) -> Result<Self> {
        let n = file.global.max_concurrent;
        let m = TtsManager(Arc::new(Inner {
            store,
            config,
            data_dir,
            boot_id: uuid::Uuid::new_v4().to_string(),
            keys: RwLock::new(HashMap::new()),
            globals: RwLock::new(file.global.clone()),
            sem: Arc::new(Semaphore::new(n)),
            sem_capacity: AtomicU64::new(n as u64),
            inflight: Mutex::new(HashMap::new()),
            queue: Mutex::new(Queue::new()),
            notify: Notify::new(),
            events: broadcast::channel(1024).0,
            breakers: Mutex::new(HashMap::new()),
            in_flight: AtomicU64::new(0),
            draining: AtomicBool::new(false),
            started: std::sync::OnceLock::new(),
        }));
        m.install_keys(file)?;
        Ok(m)
    }

    // ── configuration ──────────────────────────────────────────────────────

    /// Resolve every key against the module registry, validate its module
    /// config at LOAD, project the set into the store and refresh the queue's
    /// orders. Returns the key names that changed.
    fn install_keys(&self, file: KeysFile) -> Result<Vec<String>> {
        let mut resolved = HashMap::new();
        let mut projection = Vec::new();
        for (name, cfg) in &file.keys {
            let module = synth::module(&cfg.module).ok_or_else(|| {
                Error::Config(format!(
                    "key {name}: unknown module {:?} (known: {})",
                    cfg.module,
                    synth::MODULES.join(", ")
                ))
            })?;
            let mc = cfg.module_config_json();
            module.validate(&mc)?;
            let fingerprint = module.fingerprint(&mc);
            let ext = module.format(&mc).ext();
            projection.push((cfg.clone(), fingerprint.clone()));
            resolved.insert(name.clone(), ResolvedKey { cfg: cfg.clone(), module, module_config: mc, fingerprint, ext });
        }
        let changed = self.store.project_keys(&projection)?;
        {
            let mut q = self.queue.lock().expect("queue");
            let top = crate::priority::look_ahead(1);
            for (name, rk) in &resolved {
                if rk.cfg.priority >= top {
                    // A default at or above the ladder silently disables the
                    // ladder for that key: nothing errors, nothing reorders.
                    tracing::warn!(
                        key = %name,
                        default_priority = rk.cfg.priority,
                        ladder_top = top,
                        "cache default priority is at or above the look-ahead ladder; per-item \
                         priorities on this key cannot reorder anything (scale: content 0, system 1, \
                         ladder 2..6)"
                    );
                }
                q.set_order(name, KeyOrder { priority: rk.cfg.priority, queueing: rk.cfg.queueing });
            }
        }
        self.resize_gate(file.global.max_concurrent);
        *self.keys.write().expect("keys") = resolved;
        *self.globals.write().expect("globals") = file.global;
        Ok(changed)
    }

    /// Resize the synthesis gate LIVE. Growing adds permits; shrinking forgets
    /// free ones now and claims the rest as running syntheses hand them back.
    /// The alternative was answering `tts.reload` with "restart me", which the
    /// operator refused: *"Why are you restarting the service at all?"*
    fn resize_gate(&self, want: usize) {
        let want = want.max(1) as u64;
        let have = self.sem_capacity.swap(want, Ordering::SeqCst);
        if want > have {
            self.sem.add_permits((want - have) as usize);
        } else if want < have {
            let owe = (have - want) as usize;
            let deficit = owe - self.sem.forget_permits(owe);
            if deficit > 0 && tokio::runtime::Handle::try_current().is_ok() {
                let sem = self.sem.clone();
                tokio::spawn(async move {
                    for _ in 0..deficit {
                        match sem.clone().acquire_owned().await {
                            Ok(p) => p.forget(),
                            Err(_) => break,
                        }
                    }
                });
            }
        }
        if want != have {
            tracing::info!(from = have, to = want, "synthesis gate resized, live");
        }
    }

    pub fn globals(&self) -> GlobalConfig {
        self.globals.read().expect("globals").clone()
    }

    pub fn key(&self, name: &str) -> Result<ResolvedKey> {
        match self.keys.read().expect("keys").get(name) {
            Some(k) if k.cfg.enabled => Ok(k.clone()),
            Some(_) => Err(Error::wire("unknown_key", format!("cache key {name} is disabled"))),
            None => Err(Error::unknown_key(name)),
        }
    }

    pub fn key_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.keys.read().expect("keys").keys().cloned().collect();
        v.sort();
        v
    }

    pub fn audio_dir(&self) -> PathBuf {
        self.data_dir.join("audio")
    }

    pub fn abs_path(&self, rel: &str) -> PathBuf {
        self.audio_dir().join(rel)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Push> {
        self.events.subscribe()
    }

    fn emit(&self, p: Push) {
        let _ = self.events.send(p);
    }

    fn evicted(&self, key: &str, hash: String, bytes: u64, reason: &str) {
        let _ = self.store.bump(key, Counter::EvictedTotal, 1);
        self.emit(Push::Evicted { key: key.to_string(), hash, bytes, reason: reason.into() });
    }

    // ── the scheduler ──────────────────────────────────────────────────────

    /// Start the scheduler and the reaper. Idempotent.
    pub fn start(&self) {
        if self.started.set(()).is_err() {
            return;
        }
        let m = self.clone();
        tokio::spawn(async move { m.schedule_loop().await });
        let g = self.globals();
        if g.reap_orphans {
            let m = self.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(g.reap_interval_secs.max(1)));
                loop {
                    tick.tick().await;
                    m.reap_sweep().await;
                }
            });
        }
    }

    /// One permit, then one job, then loop.
    async fn schedule_loop(&self) {
        'outer: loop {
            let Ok(permit) = self.sem.clone().acquire_owned().await else { return };
            let job = loop {
                if self.draining.load(Ordering::SeqCst) {
                    return;
                }
                // THE PERMIT RESERVE. Warm work may start only while at least
                // RESERVED_FOR_INTERACTIVE permits stay free behind this one —
                // clamped below the gate width, or a one-permit service could
                // never warm anything at all.
                let reserve = crate::queue::RESERVED_FOR_INTERACTIVE.min(self.globals().max_concurrent.saturating_sub(1));
                let interactive_only = self.sem.available_permits() < reserve;
                let (popped, left) = {
                    let mut q = self.queue.lock().expect("queue");
                    (q.pop_where(interactive_only), q.depth())
                };
                match popped {
                    Some(j) => break j,
                    // Only warm work waits and the reserve forbids it: give the
                    // permit back — it is exactly the one a blocked caller
                    // needs — and poll, because no push is coming to wake us.
                    None if interactive_only && left > 0 => {
                        drop(permit);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        continue 'outer;
                    }
                    None => self.notify.notified().await,
                }
            };
            let m = self.clone();
            tokio::spawn(async move { m.run_job(job, permit).await });
        }
    }

    /// Stop starting work and wait, up to `limit`, for what is in flight.
    ///
    /// Called on the stop signal. The queue is in memory and dies with the
    /// process — its rows are swept cold at the next boot — but a synthesis
    /// that is running is a sentence somebody may be waiting to hear, so it is
    /// given the chance to land. Returns how many were still running.
    pub async fn drain(&self, limit: Duration) -> u64 {
        self.draining.store(true, Ordering::SeqCst);
        self.notify.notify_one();
        let deadline = Instant::now() + limit;
        while self.in_flight.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.in_flight.load(Ordering::SeqCst)
    }

    // ── registration: the PUT ──────────────────────────────────────────────

    /// `tts.register` — an idempotent PUT on `(key, hash)`.
    ///
    /// 1. Recency moves FIRST, for the whole batch, in one transaction — warm,
    ///    cold or mid-render, before any item's fate is decided. That is what
    ///    keeps a window he is navigating out of the eviction firing line, and
    ///    doing it for the whole batch first stops a window evicting its own
    ///    members.
    /// 2. The service decides each item and says what it chose.
    /// 3. The text is discarded unless the item was enqueued.
    pub async fn register(&self, key: &str, batch: Vec<RegisterItem>, lane: Lane) -> Result<Value> {
        let rk = self.key(key)?;
        let g = self.globals();
        if batch.len() > g.max_batch_items {
            return Err(Error::bad_params(format!(
                "batch of {} items exceeds max_batch_items ({})",
                batch.len(),
                g.max_batch_items
            )));
        }
        let prepared: Vec<(String, Option<String>, Option<i64>)> = batch
            .into_iter()
            .map(|it| {
                let t = it.text.trim().to_string();
                let h = (!t.is_empty()).then(|| hash_for(&t, &rk.fingerprint));
                (t, h, it.priority)
            })
            .collect();
        let mut seen = HashSet::new();
        let unique: Vec<String> =
            prepared.iter().filter_map(|(_, h, _)| h.clone()).filter(|h| seen.insert(h.clone())).collect();
        let touched = self.store.touch_recency(&rk.cfg.name, &unique)?;

        let mut items = Vec::with_capacity(prepared.len());
        let (mut warm, mut queued, mut refused, mut enqueued, mut joined, mut discarded) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        for (text, hash, priority) in prepared {
            let Some(hash) = hash else {
                refused += 1;
                items.push(json!({"hash": Value::Null, "state": "refused", "action": "refused",
                    "text_used": false, "error": "bad_params: text is empty", "code": "bad_params"}));
                continue;
            };
            match self.put(&rk, &text, &hash, lane, priority).await {
                Ok((state, action, _rx)) => {
                    let mut item = json!({"hash": hash, "state": state.name(), "action": action.as_str(),
                        "text_used": action.text_used()});
                    // Where it actually landed after defaults and the max rule.
                    if let Some(p) = self.queue.lock().expect("queue").priority_of(&rk.cfg.name, &hash) {
                        item["priority"] = json!(p);
                    }
                    if !action.text_used() {
                        discarded += 1;
                    }
                    match action {
                        PutAction::Warm => warm += 1,
                        PutAction::Joined => (joined, queued) = (joined + 1, queued + 1),
                        PutAction::Enqueued => (enqueued, queued) = (enqueued + 1, queued + 1),
                    }
                    match &state {
                        RenderState::Warm { bytes, .. } => item["bytes"] = json!(bytes),
                        // Joined a flight that had already failed.
                        RenderState::Failed { .. } => (queued, refused) = (queued.saturating_sub(1), refused + 1),
                        _ => {}
                    }
                    items.push(item);
                }
                Err(e) => {
                    refused += 1;
                    discarded += 1;
                    items.push(json!({"hash": hash, "state": "refused", "action": "refused",
                        "text_used": false, "error": e.to_string(), "code": e.code()}));
                }
            }
        }
        self.emit_queue_depth(key);
        Ok(json!({"items": items, "warm": warm, "queued": queued, "refused": refused,
            "enqueued": enqueued, "joined": joined, "touched": touched, "discarded": discarded}))
    }

    /// The single-flight core: probe, claim or join, admit, enqueue.
    ///
    /// Returns the claim's OWN receiver, taken while the claim is held: a
    /// caller that looked the claim up again afterwards could find it already
    /// gone, and would then have only the database to ask — which is how a
    /// stored failure once came to answer a live request.
    ///
    /// Admission and the breaker gate WORK, and only the cold branch makes
    /// any: a join costs nothing, so it is answered even when the key is full
    /// or the module degraded. Everything after the claim runs with the claim
    /// held, so every failure there releases it, at one site.
    async fn put(
        &self,
        rk: &ResolvedKey,
        text: &str,
        h: &str,
        lane: Lane,
        priority: Option<i64>,
    ) -> Result<(RenderState, PutAction, Option<watch::Receiver<RenderState>>)> {
        if let Some(state) = self.probe_warm(h)? {
            self.store.register_entry(&rk.cfg.name, h)?;
            return Ok((state, PutAction::Warm, None));
        }
        // (receiver, true if this call created the claim)
        let (rx, mine) = {
            let mut fl = self.inflight.lock().expect("inflight");
            match fl.get(h) {
                Some(f) => (f.rx.clone(), false),
                None => {
                    // Re-probe under the lock, so the winner's write cannot
                    // slip between the probe and the claim.
                    if let Some(state) = self.probe_warm(h)? {
                        drop(fl);
                        self.store.register_entry(&rk.cfg.name, h)?;
                        return Ok((state, PutAction::Warm, None));
                    }
                    let (tx, rx) = watch::channel(RenderState::Queued);
                    let (cancel, cancel_rx) = CancelHandle::new();
                    fl.insert(h.to_string(), Flight { tx: Arc::new(tx), rx: rx.clone(), cancel, cancel_rx });
                    (rx, true)
                }
            }
        };
        if !mine {
            self.store.register_entry(&rk.cfg.name, h)?;
            let mut q = self.queue.lock().expect("queue");
            q.raise_priority(&rk.cfg.name, h, priority);
            if lane == Lane::Interactive {
                q.promote(&rk.cfg.name, h);
            }
            let state = rx.borrow().clone();
            return Ok((state, PutAction::Joined, Some(rx)));
        }

        let enqueued = (|| -> Result<Vec<Job>> {
            self.admit(rk)?;
            self.breaker_gate(rk.module.name())?;
            self.store.ensure_render(h, rk.module.name(), &rk.fingerprint, text.chars().count(), rk.ext)?;
            self.store.register_entry(&rk.cfg.name, h)?;
            let job = Job { key: rk.cfg.name.clone(), hash: h.into(), text: text.into(), lane, priority, attempt: 0 };
            Ok(self.queue.lock().expect("queue").push(job))
        })();
        match enqueued {
            Ok(evicted) => {
                for job in evicted {
                    self.release_evicted(&job);
                }
                self.notify.notify_one();
                Ok((RenderState::Queued, PutAction::Enqueued, Some(rx)))
            }
            Err(e) => {
                self.finish(h, RenderState::failed(e.code().unwrap_or("refused"), e.to_string()));
                Err(e)
            }
        }
    }

    /// Release what an evicted job left behind: its claim and its store row.
    /// Eviction means "not now" — a fact about the queue, not the text — so the
    /// row is deleted rather than marked failed, and the text is merely cold.
    fn release_evicted(&self, job: &Job) {
        self.finish(&job.hash, RenderState::failed("evicted", "dropped from the queue under pressure; ask again"));
        let _ = self.store.clear_unfinished_render(&job.hash);
        // No audio existed: this render never reached a backend.
        self.evicted(&job.key, job.hash.clone(), 0, "queue_pressure");
    }

    /// `tts.cancel` — let go of work the client no longer wants.
    ///
    /// Queued: taken out and released exactly as an evicted job is. In flight:
    /// its cancel fires, the backend group is torn down and the permit freed at
    /// once. Warm, failed or unknown: nothing to do, and warm audio is KEPT.
    /// Cancellation is by hash, so two rows with identical text share it; with
    /// one client cancelling only what it abandoned, that is theoretical.
    pub fn cancel(&self, key: &str, hashes: &[String]) -> Result<Value> {
        let rk = self.key(key)?;
        if let Some(bad) = hashes.iter().find(|h| !is_hash(h)) {
            return Err(Error::bad_params(format!("malformed hash: {bad}")));
        }
        let (mut dequeued, mut aborted, mut unknown) = (0u64, 0u64, 0u64);
        for hash in hashes {
            let job = self.queue.lock().expect("queue").remove(&rk.cfg.name, hash);
            if let Some(job) = job {
                self.finish(&job.hash, RenderState::failed("cancelled", "withdrawn by client before synthesis"));
                let _ = self.store.clear_unfinished_render(&job.hash);
                dequeued += 1;
                continue;
            }
            // In flight, if a claim exists: fire it, and let run_job release
            // the claim when the backend unwinds.
            let handle = self.inflight.lock().expect("inflight").get(hash).map(|f| f.cancel.clone());
            match handle {
                Some(h) if h.cancel() => aborted += 1,
                _ => unknown += 1,
            }
        }
        Ok(json!({"dequeued": dequeued, "aborted": aborted, "unknown": unknown}))
    }

    /// Warm iff the row says warm AND the bytes are on disk. A row naming a
    /// missing file is a message he would not hear, so it is repaired here.
    fn probe_warm(&self, h: &str) -> Result<Option<RenderState>> {
        let Some(r) = self.store.get_render(h)? else { return Ok(None) };
        let (true, Some(rel)) = (r.state == "warm", r.rel_path) else { return Ok(None) };
        match std::fs::metadata(self.abs_path(&rel)) {
            Ok(meta) if meta.len() > 0 => {
                Ok(Some(RenderState::Warm { rel_path: rel, bytes: r.bytes, synth_ms: r.synth_ms.unwrap_or(0) }))
            }
            _ => {
                self.store.forget_missing_render(h)?;
                Ok(None)
            }
        }
    }

    /// The keep-warm ceiling, and the global disk backstop.
    fn admit(&self, rk: &ResolvedKey) -> Result<()> {
        let g = self.globals();
        if rk.cfg.mode == Mode::KeepWarm {
            let bytes = self.store.key_stats(&rk.cfg.name)?.bytes;
            let pressure = |limit, kind: &str| Push::CachePressure { key: rk.cfg.name.clone(), bytes, limit, kind: kind.into() };
            if let Some(refuse) = rk.cfg.refuse_bytes.filter(|r| bytes >= *r) {
                self.emit(pressure(refuse, "refuse"));
                return Err(Error::wire("key_full", format!("{} is at its keep-warm ceiling", rk.cfg.name)));
            }
            if let Some(warn) = rk.cfg.warn_bytes.filter(|w| bytes >= *w) {
                tracing::warn!(key = %rk.cfg.name, bytes, limit = warn, "keep-warm key past its warn threshold");
                self.emit(pressure(warn, "warn"));
            }
        }
        let total = self.store.total_warm_bytes()?;
        if total >= g.max_bytes {
            // Evict hardest from the largest LRU key first; refuse only if
            // that is not enough.
            self.enforce_global()?;
            if self.store.total_warm_bytes()? >= g.max_bytes {
                self.emit(Push::CachePressure { key: rk.cfg.name.clone(), bytes: total, limit: g.max_bytes, kind: "global".into() });
                return Err(Error::wire("disk_full", format!("global cache is at {total} of {} bytes", g.max_bytes)));
            }
        }
        Ok(())
    }

    // ── the job ────────────────────────────────────────────────────────────

    async fn run_job(&self, job: Job, permit: OwnedSemaphorePermit) {
        let Ok(rk) = self.key(&job.key) else {
            self.finish(&job.hash, RenderState::failed("unknown_key", format!("key {} vanished", job.key)));
            return;
        };
        // A permit is never spent on work that finished while we queued.
        if let Ok(Some(state)) = self.probe_warm(&job.hash) {
            self.finish(&job.hash, state);
            return;
        }
        // Cross-process single-flight: a lock held by a live peer is
        // respected and its result awaited; a dead or previous holder's is
        // stolen.
        match self.store.claim_synth_lock(&job.hash, &self.boot_id, std::process::id(), job.lane.as_str()) {
            Ok(true) => {}
            Ok(false) => {
                drop(permit);
                self.await_foreign_synthesis(&rk, &job).await;
                return;
            }
            Err(e) => return self.fail_job(&rk, &job, "synthesis_failed", &format!("lock: {e}")),
        }
        let _ = self.store.set_render_state(&job.hash, "synthesizing");
        self.publish(&job.hash, RenderState::Synthesizing);
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        self.emit_queue_depth(&job.key);

        let rel = hash::rel_path(&job.hash, rk.ext);
        let out = self.abs_path(&rel);
        let g = self.globals();
        let started = Instant::now();
        // The withdrawal token belongs to the CLAIM, so a cancel that arrived
        // between the pop and here is already visible.
        let cancel = self.inflight.lock().expect("inflight").get(&job.hash).map(|f| f.cancel_rx.clone());
        let res = rk
            .module
            .synthesize(&job.text, &rk.module_config, &out, g.synth_timeout_ms, cancel.unwrap_or_else(Cancel::never))
            .await;
        let synth_ms = started.elapsed().as_millis() as u64;
        let _ = self.store.release_synth_lock(&job.hash);
        drop(permit);

        match res {
            Ok(o) if o.bytes > g.max_entry_bytes => {
                // A runaway backend is REPORTED, never silently dropped.
                let _ = std::fs::remove_file(&out);
                let why = format!("{} bytes exceeds max_entry_bytes ({})", o.bytes, g.max_entry_bytes);
                self.fail_job(&rk, &job, "too_large", &why);
            }
            Ok(o) => {
                let _ = self.store.mark_warm(&job.hash, &rel, o.bytes, rk.ext, synth_ms);
                let _ = self.store.bump(&job.key, Counter::SynthTotal, 1);
                let _ = self.store.bump(&job.key, Counter::BytesWritten, o.bytes as i64);
                self.breaker_success(rk.module.name());
                self.finish(&job.hash, RenderState::Warm { rel_path: rel, bytes: o.bytes, synth_ms });
                self.emit(Push::Warmed {
                    key: job.key.clone(),
                    hash: job.hash.clone(),
                    bytes: o.bytes,
                    synth_ms,
                    module: rk.module.name().into(),
                });
                let _ = self.enforce_key(&rk);
                let _ = self.enforce_global();
            }
            // WITHDRAWN, which is not a failure: no durable row, no breaker
            // mark, no failure count — "he moved the cursor" must never trip
            // the backend as degraded. The claim is released so nobody stays
            // parked, and the text is cold again.
            Err(e) if e.code() == Some("cancelled") => {
                self.finish(&job.hash, RenderState::failed("cancelled", "withdrawn by client"));
                let _ = self.store.clear_unfinished_render(&job.hash);
            }
            // INTERRUPTED: the machine took the process away. Same law as a
            // cancel, but somebody may still be waiting to hear this, so the
            // job goes back on the queue — claim, lane and priority intact.
            Err(Error::Interrupted { signal }) if job.attempt < INTERRUPTED_REQUEUES => {
                tracing::warn!(hash = %job.hash, key = %job.key, signal, attempt = job.attempt,
                    "synthesis child killed by a signal from outside the service; requeuing — not a failure of the text");
                let _ = self.store.set_render_state(&job.hash, "queued");
                self.publish(&job.hash, RenderState::Queued);
                let evicted = self.queue.lock().expect("queue").push(Job { attempt: job.attempt + 1, ..job.clone() });
                for j in evicted {
                    self.release_evicted(&j);
                }
                self.notify.notify_one();
            }
            // Interrupted again: stop, the way a withdrawal stops. The code is
            // internal; `request` maps it to `synthesis_failed`, which is true
            // of this attempt and retryable, where `cancelled` would be a lie.
            Err(Error::Interrupted { signal }) => {
                tracing::error!(hash = %job.hash, key = %job.key, signal, attempts = job.attempt + 1,
                    "synthesis killed by a signal on every attempt; releasing the claim cold");
                self.finish(
                    &job.hash,
                    RenderState::failed(
                        "interrupted",
                        format!(
                            "synthesis was killed by signal {signal} on {} attempts; nothing is wrong with the \
                             text — ask again",
                            job.attempt + 1
                        ),
                    ),
                );
                let _ = self.store.clear_unfinished_render(&job.hash);
            }
            Err(e) => {
                let detail = e.to_string();
                self.breaker_failure(rk.module.name(), &detail);
                self.fail_job(&rk, &job, e.code().unwrap_or("synthesis_failed"), &detail);
            }
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.emit_queue_depth(&job.key);
    }

    /// A live peer daemon holds the lock: wait for ITS result rather than run
    /// a second `say` for the same utterance.
    async fn await_foreign_synthesis(&self, rk: &ResolvedKey, job: &Job) {
        let deadline = Instant::now() + Duration::from_millis(self.globals().synth_timeout_ms);
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Ok(Some(state)) = self.probe_warm(&job.hash) {
                return self.finish(&job.hash, state);
            }
        }
        self.fail_job(rk, job, "synthesis_failed", "another daemon holds this synthesis and did not finish");
    }

    fn fail_job(&self, rk: &ResolvedKey, job: &Job, code: &str, reason: &str) {
        let _ = self.store.mark_failed(&job.hash, code, reason);
        let _ = self.store.bump(&job.key, Counter::FailTotal, 1);
        let _ = self.store.release_synth_lock(&job.hash);
        self.finish(&job.hash, RenderState::failed(code, reason));
        self.emit(Push::SynthesisFailed {
            key: job.key.clone(),
            hash: job.hash.clone(),
            code: code.into(),
            reason: reason.into(),
            module: rk.module.name().into(),
        });
    }

    /// A non-terminal state, to every waiter.
    fn publish(&self, hash: &str, state: RenderState) {
        let tx = self.inflight.lock().expect("inflight").get(hash).map(|f| f.tx.clone());
        if let Some(tx) = tx {
            let _ = tx.send(state);
        }
    }

    /// A TERMINAL state, and the claim released. The value is sent before the
    /// sender drops, and `watch` compares versions before it reports closure,
    /// so a waiter parked on `changed()` sees the state, not a closed channel.
    fn finish(&self, hash: &str, state: RenderState) {
        if let Some(f) = self.inflight.lock().expect("inflight").remove(hash) {
            let _ = f.tx.send(state);
        }
    }

    // ── the circuit breaker ────────────────────────────────────────────────

    fn breaker_gate(&self, module: &str) -> Result<()> {
        let g = self.globals();
        if !g.breaker_enabled {
            return Ok(());
        }
        let mut b = self.breakers.lock().expect("breakers");
        let e = b.entry(module.to_string()).or_default();
        if e.degraded_since.is_none() {
            return Ok(());
        }
        let now = Instant::now();
        if e.next_probe.is_some_and(|t| now >= t) {
            // THIS request is the probe. Let it through and re-arm.
            e.next_probe = Some(now + Duration::from_secs(g.breaker_probe_secs));
            return Ok(());
        }
        Err(Error::wire(
            "backend_unavailable",
            format!("module {module} is degraded: {}", e.reason.as_deref().unwrap_or("consecutive failures")),
        ))
    }

    fn breaker_failure(&self, module: &str, reason: &str) {
        let g = self.globals();
        let mut b = self.breakers.lock().expect("breakers");
        let e = b.entry(module.to_string()).or_default();
        e.consecutive += 1;
        e.reason = Some(reason.to_string());
        // With the breaker off the count still runs, but it never OPENS:
        // announcing "degraded" while every request goes through is a lie.
        if g.breaker_enabled && e.consecutive >= g.breaker_threshold && e.degraded_since.is_none() {
            let now = Instant::now();
            e.degraded_since = Some(now);
            e.next_probe = Some(now + Duration::from_secs(g.breaker_probe_secs));
            drop(b);
            tracing::error!(module, reason, "module degraded — circuit breaker open");
            self.emit(Push::ModuleHealth { module: module.into(), state: "degraded".into(), reason: Some(reason.into()) });
        }
    }

    fn breaker_success(&self, module: &str) {
        let was = self.breakers.lock().expect("breakers").insert(module.to_string(), Breaker::default());
        if was.is_some_and(|b| b.degraded_since.is_some()) {
            self.emit(Push::ModuleHealth { module: module.into(), state: "ok".into(), reason: None });
        }
    }

    pub fn module_health(&self) -> Value {
        let b = self.breakers.lock().expect("breakers");
        Value::Object(
            synth::MODULES
                .iter()
                .map(|name| {
                    let st = b.get(*name);
                    let degraded = st.is_some_and(|s| s.degraded_since.is_some());
                    (
                        name.to_string(),
                        json!({
                            "state": if degraded { "degraded" } else { "ok" },
                            "consecutive_failures": st.map_or(0, |s| s.consecutive),
                            "reason": st.and_then(|s| s.reason.clone()),
                        }),
                    )
                })
                .collect(),
        )
    }

    // ── eviction ───────────────────────────────────────────────────────────

    /// Per-key eviction. A `keep_warm` key is never scanned for victims. The
    /// two bounded modes are one policy — least recently used first, nothing
    /// younger than `min_age_secs`, rows committed before bytes unlinked — with
    /// a different unit on the budget: warm BYTES for `lru`, warm ENTRIES for
    /// `lru_count`. Warm entries, because a queued or failed render holds no
    /// bytes and counting it would evict the value he can actually hear. A
    /// count of one therefore evicts the old value on every write, and the
    /// entry that just arrived is never its own victim.
    pub fn enforce_key(&self, rk: &ResolvedKey) -> Result<u64> {
        let stats = self.store.key_stats(&rk.cfg.name)?;
        let (budget, mut held, by_bytes, reason) = match (rk.cfg.mode, rk.cfg.max_bytes, rk.cfg.max_entries) {
            (Mode::Lru, Some(b), _) => (b, stats.bytes, true, "lru"),
            (Mode::LruCount, _, Some(n)) => (n, stats.warm, false, "lru_count"),
            _ => return Ok(0),
        };
        if held <= budget {
            return Ok(0);
        }
        let mut freed = 0;
        for (hash, bytes) in self.store.lru_candidates(&rk.cfg.name, rk.cfg.min_age_secs)? {
            if held <= budget {
                break;
            }
            freed += self.store.drop_entry(&rk.cfg.name, &hash)?;
            held = held.saturating_sub(if by_bytes { bytes } else { 1 });
            self.evicted(&rk.cfg.name, hash, bytes, reason);
        }
        self.drain_unlinks();
        Ok(freed)
    }

    /// The global backstop: past `global.max_bytes`, evict from the largest
    /// evictable key first. Never a `keep_warm` entry.
    pub fn enforce_global(&self) -> Result<u64> {
        let g = self.globals();
        let mut total = self.store.total_warm_bytes()?;
        if total <= g.max_bytes {
            return Ok(0);
        }
        let candidates: Vec<ResolvedKey> = self
            .keys
            .read()
            .expect("keys")
            .values()
            .filter(|k| k.cfg.mode.is_evictable() && k.cfg.enabled)
            .cloned()
            .collect();
        let mut sized = Vec::new();
        for k in candidates {
            sized.push((self.store.key_stats(&k.cfg.name)?.bytes, k));
        }
        sized.sort_by(|a, b| b.0.cmp(&a.0));
        let mut freed = 0;
        'keys: for (_, rk) in sized {
            for (hash, bytes) in self.store.lru_candidates(&rk.cfg.name, rk.cfg.min_age_secs)? {
                if total <= g.max_bytes {
                    break 'keys;
                }
                let f = self.store.drop_entry(&rk.cfg.name, &hash)?;
                total = total.saturating_sub(f);
                freed += f;
                self.evicted(&rk.cfg.name, hash, bytes, "global");
            }
        }
        self.emit(Push::CachePressure { key: String::new(), bytes: total, limit: g.max_bytes, kind: "global".into() });
        self.drain_unlinks();
        Ok(freed)
    }

    /// Unlink files whose rows are already committed away.
    pub fn drain_unlinks(&self) {
        for (rel, _) in self.store.take_pending_unlink().unwrap_or_default() {
            let _ = std::fs::remove_file(self.abs_path(&rel));
            let _ = self.store.clear_pending_unlink(&rel);
        }
    }

    // ── the wire methods ───────────────────────────────────────────────────

    /// `tts.request` — "if it's warm it returns right away. If it's not warm,
    /// then we can generate it on the fly and keep it warm."
    pub async fn request(&self, key: &str, hash_in: Option<String>, text: Option<String>, wait_ms: u64, mode: &str) -> Result<Value> {
        match mode {
            "path" => {}
            "bytes" => {
                return Err(Error::bad_params(
                    "mode: \"bytes\" is reserved in the protocol and unimplemented — the service and its \
                     client share a machine and a mean entry is ~586 KB",
                ))
            }
            other => return Err(Error::bad_params(format!("unknown mode: {other}"))),
        }
        let rk = self.key(key)?;
        // Both may be sent; a disagreement is an ERROR, never a silent pick.
        let hash = match (&hash_in, &text) {
            (Some(h), _) if !is_hash(h) => return Err(Error::bad_params(format!("malformed hash: {h}"))),
            (Some(h), Some(t)) => {
                let computed = hash_for(t.trim(), &rk.fingerprint);
                if &computed != h {
                    return Err(Error::wire("hash_mismatch", format!("client sent {h} for text hashing to {computed}")));
                }
                computed
            }
            (Some(h), None) => h.clone(),
            (None, Some(t)) => hash_for(t.trim(), &rk.fingerprint),
            (None, None) => return Err(Error::bad_params("one of hash or text is required")),
        };
        let start = Instant::now();
        if let Some(state) = self.probe_warm(&hash)? {
            self.store.register_entry(&rk.cfg.name, &hash)?;
            self.store.touch(&rk.cfg.name, std::slice::from_ref(&hash))?;
            return Ok(self.warm_payload(&hash, &state, true, start));
        }
        let Some(text) = text else {
            return Err(Error::wire("unknown_hash", format!("{hash} was never registered and no text was supplied")));
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(Error::bad_params("text is empty"));
        }
        // Taken before this job is pushed: after the fact it is gone.
        let miss = self.miss_snapshot(&rk.cfg.name);
        self.store.touch_recency(&rk.cfg.name, std::slice::from_ref(&hash))?;
        // No priority: a blocked caller is known by its lane (INTERACTIVE),
        // and `None` still raises a hash queued by a warm sweep to at least
        // its cache default before promoting it.
        let (mut state, _, rx) = self.put(&rk, trimmed, &hash, Lane::Interactive, None).await?;
        if let (false, Some(mut rx)) = (state.is_terminal(), rx) {
            let waited = tokio::time::timeout(Duration::from_millis(wait_ms), async {
                loop {
                    let s = rx.borrow_and_update().clone();
                    if s.is_terminal() || rx.changed().await.is_err() {
                        return s;
                    }
                }
            })
            .await;
            match waited {
                Ok(s) => state = s,
                Err(_) => {
                    let mut details = json!({"hash": hash, "state": self.live_state_name(&hash),
                        "queue_position": self.queue_position(&hash)});
                    miss.merge_into(&mut details);
                    return Err(Error::wire_with("not_ready", format!("{hash} is not warm after {wait_ms}ms"), details));
                }
            }
        }
        if !state.is_terminal() {
            // The sender went away unseen. The store may answer WARM — bytes
            // on disk are a fact — and that is all it may answer: a stored
            // failure never answers a live request.
            if let Some(s) = self.probe_warm(&hash)? {
                state = s;
            }
        }
        match state {
            RenderState::Warm { .. } => {
                self.store.touch(&rk.cfg.name, std::slice::from_ref(&hash))?;
                let mut v = self.warm_payload(&hash, &state, false, start);
                miss.merge_into(&mut v);
                Ok(v)
            }
            RenderState::Failed { code, reason } => Err(Error::wire(
                match code.as_str() {
                    "too_large" => "too_large",
                    "backend_unavailable" => "backend_unavailable",
                    "cancelled" => "cancelled",
                    "evicted" => "not_ready",
                    _ => "synthesis_failed",
                },
                reason,
            )),
            other => Err(Error::wire_with(
                "not_ready",
                format!("{hash} is {}", other.name()),
                json!({"hash": hash, "state": other.name(), "queue_position": self.queue_position(&hash)}),
            )),
        }
    }

    fn live_state_name(&self, hash: &str) -> String {
        self.store.get_render(hash).ok().flatten().map_or_else(|| "queued".into(), |r| r.state)
    }

    fn miss_snapshot(&self, key: &str) -> MissSnapshot {
        let (depth, depth_for_key) = {
            let q = self.queue.lock().expect("queue");
            (q.depth(), q.depth_for(key))
        };
        let gate_max = self.globals().max_concurrent;
        MissSnapshot {
            queued_behind: depth,
            queued_behind_key: depth_for_key,
            gate_busy: gate_max.saturating_sub(self.sem.available_permits()) as u64,
            gate_max: gate_max as u64,
        }
    }

    fn warm_payload(&self, hash: &str, state: &RenderState, from_cache: bool, start: Instant) -> Value {
        let RenderState::Warm { rel_path, bytes, synth_ms } = state else {
            unreachable!("warm_payload on a non-warm state")
        };
        // A PATH, not bytes: same machine, and the face plays a path.
        json!({"hash": hash, "state": "warm", "path": self.abs_path(rel_path).to_string_lossy(),
            "bytes": bytes, "format": "aiff", "from_cache": from_cache, "synth_ms": synth_ms,
            "age_ms": start.elapsed().as_millis() as u64})
    }

    /// `tts.touch` — recency for what he is LOOKING at but not playing.
    pub fn touch(&self, key: &str, hashes: &[String]) -> Result<Value> {
        self.key(key)?;
        let (touched, unknown) = self.store.touch(key, hashes)?;
        Ok(json!({"touched": touched, "unknown": unknown}))
    }

    /// `tts.forget` — the only sanctioned deletion path.
    pub fn forget(&self, key: &str, hashes: &[String]) -> Result<Value> {
        self.key(key)?;
        let (mut forgotten, mut freed) = (0u64, 0u64);
        for h in hashes {
            let bytes = self.store.drop_entry(key, h)?;
            if bytes > 0 || self.store.entry(key, h)?.is_none() {
                forgotten += 1;
            }
            freed += bytes;
            self.emit(Push::Evicted { key: key.into(), hash: h.clone(), bytes, reason: "forget".into() });
        }
        self.drain_unlinks();
        Ok(json!({"forgotten": forgotten, "freed_bytes": freed}))
    }

    /// `tts.keys` — so a client validates its key names once, at startup.
    pub fn keys_payload(&self) -> Result<Value> {
        Ok(json!({"keys": self.store.list_key_rows()?}))
    }

    pub fn queue_depth(&self) -> u64 {
        self.queue.lock().expect("queue").depth()
    }

    /// Where `hash` sits in the serve order; `None` once it is not waiting.
    pub fn queue_position(&self, hash: &str) -> Option<u64> {
        self.queue.lock().expect("queue").position(hash)
    }

    /// `tts.status` — the observability the incident did not have.
    pub fn status(&self, key: Option<&str>) -> Result<Value> {
        let g = self.globals();
        let names = match key {
            Some(k) => {
                self.key(k)?;
                vec![k.to_string()]
            }
            None => self.key_names(),
        };
        let mut per_key = Vec::new();
        for name in names {
            let rk = self.key(&name)?;
            let (s, c) = (self.store.key_stats(&name)?, self.store.counters(&name)?);
            per_key.push(json!({
                "key": name, "mode": rk.cfg.mode.as_str(), "module": rk.cfg.module,
                "queueing": rk.cfg.queueing.as_str(), "max_bytes": rk.cfg.max_bytes,
                "max_entries": rk.cfg.max_entries, "warn_bytes": rk.cfg.warn_bytes,
                "refuse_bytes": rk.cfg.refuse_bytes, "min_age_secs": rk.cfg.min_age_secs,
                "priority": rk.cfg.priority, "entries": s.entries, "bytes": s.bytes, "warm": s.warm,
                "queued": s.queued, "synthesizing": s.synthesizing, "failed": s.failed,
                "queue_depth": self.queue.lock().expect("queue").depth_for(&name),
                "evicted_total": c.evicted_total, "synth_total": c.synth_total,
                "fail_total": c.fail_total, "bytes_written": c.bytes_written,
                "oldest_used_at": s.oldest_used_at,
            }));
        }
        Ok(json!({
            "keys": per_key,
            "global": {
                "max_concurrent": g.max_concurrent,
                "in_flight": self.in_flight.load(Ordering::SeqCst),
                "queue_depth": self.queue_depth(),
                "disk_bytes": self.store.total_warm_bytes()?,
                "disk_max": g.max_bytes,
                "breaker_enabled": g.breaker_enabled,
                "module_health": self.module_health(),
                "boot_id": self.boot_id,
            }
        }))
    }

    fn keys_path(&self) -> PathBuf {
        self.data_dir.join("tts.toml")
    }

    /// `tts.reload` — re-read `tts.toml`. A key gone from the file is disabled,
    /// never purged, and nothing here can answer "restart me": the gate width
    /// is resized live.
    pub fn reload(&self) -> Result<Value> {
        let file = KeysFile::load_or_seed(&self.keys_path())?;
        let (before, after) = (self.globals().max_concurrent, file.global.max_concurrent);
        let changed = self.install_keys(file)?;
        Ok(json!({"keys": self.store.list_key_rows()?, "changed": changed,
            "max_concurrent": after, "max_concurrent_was": before}))
    }

    /// `tts.upsert_key` — create a key and its config from the client, at
    /// runtime. "If it does not have the cache, then it should create it."
    ///
    /// Idempotent by default: an existing key is left exactly as it is (a
    /// client declares its keys on every boot and must never clobber what the
    /// operator tuned). `replace` rewrites it. Either way the key is written
    /// into `tts.toml`, comments intact, AND installed in the running daemon.
    pub fn upsert_key(&self, name: &str, config: &Value, replace: bool) -> Result<Value> {
        let path = self.keys_path();
        let current = KeysFile::load_or_seed(&path)?;
        if let Some(existing) = current.keys.get(name).filter(|_| !replace) {
            return Ok(json!({"key": name, "created": false, "updated": false, "config": existing.to_json()}));
        }
        let existed = current.keys.contains_key(name);
        let cfg = KeyConfig::from_json(name, config)?;
        // Validated BEFORE anything is written, so a bad module config cannot
        // leave a stanza on disk that the daemon then refuses to boot on.
        let module = synth::module(&cfg.module).ok_or_else(|| {
            Error::bad_params(format!("key {name}: unknown module {:?} (known: {})", cfg.module, synth::MODULES.join(", ")))
        })?;
        module.validate(&cfg.module_config_json())?;
        let text = std::fs::read_to_string(&path).map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
        let next_text = crate::keys::upsert_key_text(&text, &cfg)?;
        // The edited file must parse as a whole before it replaces the old one:
        // a tts.toml this daemon cannot read is the one failure that would
        // genuinely need a human.
        let next = KeysFile::from_toml(&next_text)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, &next_text).map_err(|e| Error::Config(format!("writing {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::Config(format!("renaming into {}: {e}", path.display())))?;
        let changed = self.install_keys(next)?;
        tracing::info!(key = name, created = !existed, "cache key upserted at runtime");
        Ok(json!({"key": name, "created": !existed, "updated": existed, "changed": changed, "config": cfg.to_json()}))
    }

    fn emit_queue_depth(&self, key: &str) {
        let depth = self.queue.lock().expect("queue").depth_for(key);
        self.emit(Push::QueueDepth { key: key.into(), depth, in_flight: self.in_flight.load(Ordering::SeqCst) });
    }

    // ── boot ───────────────────────────────────────────────────────────────

    /// Boot recovery, non-destructive by construction.
    ///
    /// Locks from another boot are cleared; `synthesizing`, `queued` and
    /// `failed` rows — the three states a dead process can strand — are swept
    /// cold, because nothing survives a boot that intends to finish them;
    /// pending unlinks are drained; stale `.part` files are removed; and the
    /// files and rows are reconciled in both directions, with a HARD REFUSAL to
    /// delete more than `boot_sweep_max_delete_pct` of the audio directory in
    /// one pass unless `TTS_ALLOW_BULK_SWEEP=1` ("It might've even deleted all
    /// of my WAV files").
    pub fn boot_sweep(&self) -> Result<Value> {
        std::fs::create_dir_all(self.audio_dir())?;
        let cleared_locks = self.store.clear_locks_not_from(&self.boot_id)?;
        // ORDER: the requeue turns `synthesizing` into `queued` so the next
        // sweep collects both.
        let requeued = self.store.requeue_orphaned_synthesizing()?;
        let stranded = self.store.clear_stranded("queued")?;
        let stale_failed = self.store.clear_stranded("failed")?;
        self.drain_unlinks();

        let g = self.globals();
        let known = self.store.warm_paths()?;
        let known_rel: HashSet<&String> = known.values().collect();
        let (mut on_disk, mut stale_parts) = (Vec::new(), 0u64);
        walk(&self.audio_dir(), &self.audio_dir(), &mut |rel, meta| {
            if rel.ends_with(".part.aiff") {
                let age = meta.modified().ok().and_then(|m| m.elapsed().ok()).unwrap_or_default();
                if age.as_secs() >= g.stale_part_secs && std::fs::remove_file(self.abs_path(rel)).is_ok() {
                    stale_parts += 1;
                }
            } else {
                on_disk.push(rel.to_string());
            }
        });
        let orphans: Vec<&String> = on_disk.iter().filter(|r| !known_rel.contains(r)).collect();
        let missing: Vec<&String> = known.iter().filter(|(_, rel)| !self.abs_path(rel).exists()).map(|(h, _)| h).collect();
        let pct = orphans.len() * 100 / on_disk.len().max(1);
        let refused = pct as u32 > g.boot_sweep_max_delete_pct && std::env::var("TTS_ALLOW_BULK_SWEEP").as_deref() != Ok("1");
        let mut unlinked = 0u64;
        if refused {
            tracing::error!(orphans = orphans.len(), total = on_disk.len(), pct, limit = g.boot_sweep_max_delete_pct,
                "boot sweep REFUSED: it would delete more than the allowed fraction of the audio directory; \
                 set TTS_ALLOW_BULK_SWEEP=1 if that is really meant");
        } else {
            unlinked = orphans.iter().filter(|rel| std::fs::remove_file(self.abs_path(rel)).is_ok()).count() as u64;
        }
        for h in &missing {
            let _ = self.store.forget_missing_render(h);
        }
        Ok(json!({
            "cleared_locks": cleared_locks, "requeued": requeued,
            "stranded_queued_cleared": stranded, "stale_failed_cleared": stale_failed,
            "stale_parts_removed": stale_parts, "orphan_files": orphans.len(),
            "orphan_files_unlinked": unlinked, "orphan_pct": pct, "sweep_refused": refused,
            "missing_rows_dropped": missing.len(),
        }))
    }

    /// The orphan reaper: kill `say` processes writing into THIS service's
    /// audio directory that this daemon does not own. The 11 orphans that
    /// motivated it sat at ppid 1 for 42 hours costing every other synthesis
    /// 1.6 s. Ownership is read on both sides of the `ps` call, so a child that
    /// starts or ends meanwhile is still recognised as ours: missing a real
    /// orphan costs one late reap; killing our own child costs a sentence.
    pub async fn reap_sweep(&self) -> u64 {
        if !self.globals().reap_orphans {
            return 0;
        }
        let mut owned = synth::system::live_child_pids();
        let Ok(out) = tokio::process::Command::new("ps").args(["-eo", "pid=,command="]).output().await else {
            return 0;
        };
        owned.extend(synth::system::live_child_pids());
        let victims = orphan_pids(&String::from_utf8_lossy(&out.stdout), &self.audio_dir(), std::process::id(), &owned);
        for pid in &victims {
            tracing::warn!(pid, "reaping an orphaned synthesis writing into our audio dir");
            synth::system::kill_group(*pid);
        }
        victims.len() as u64
    }
}

/// The reaper's selector, pure so its dangerous half is testable with no
/// process at all: `say` invocations whose `-o` target lies under
/// `audio_dir`, that are neither this daemon nor a child it owns.
pub fn orphan_pids(ps_output: &str, audio_dir: &Path, self_pid: u32, owned: &[u32]) -> Vec<u32> {
    let dir = audio_dir.to_string_lossy();
    ps_output
        .lines()
        .filter_map(|line| {
            let (pid, cmd) = line.trim().split_once(char::is_whitespace)?;
            let pid: u32 = pid.trim().parse().ok()?;
            let cmd = cmd.trim();
            let is_say = cmd.starts_with("say ") || cmd.contains("/say ");
            let target = cmd.split(" -o ").nth(1)?.split_whitespace().next()?;
            (pid != self_pid && !owned.contains(&pid) && is_say && target.starts_with(&*dir)).then_some(pid)
        })
        .collect()
}

fn walk(root: &Path, dir: &Path, f: &mut impl FnMut(&str, &std::fs::Metadata)) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(meta) = e.metadata() else { continue };
        let p = e.path();
        if meta.is_dir() {
            walk(root, &p, f);
        } else if let Ok(rel) = p.strip_prefix(root) {
            f(&rel.to_string_lossy(), &meta);
        }
    }
}

/// A file's mtime as RFC 3339: the importer's recency seed.
pub fn mtime_rfc3339(meta: &std::fs::Metadata) -> String {
    chrono::DateTime::<chrono::Utc>::from(meta.modified().unwrap_or(std::time::UNIX_EPOCH)).to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = "\
  101 say -o /tmp/tts-home/audio/ab/cd/deadbeef.part.aiff -v Samantha -- hello
  102 say -o /Users/x/.aui/tts_cache/other.aiff -v Samantha -- not ours
  103 /usr/bin/say -o /tmp/tts-home/audio/ff/ee/beef.part.aiff -v Samantha -- also ours
  104 afplay /tmp/tts-home/audio/ab/cd/deadbeef.aiff
  105 tts --config /tmp/tts-home/config.toml
  107 say -o /tmp/tts-home/audio/11/22/orphan.part.aiff -v Samantha -- a real orphan
";

    /// Only `say` writing into OUR audio dir, never the daemon itself.
    #[test]
    fn the_reaper_selects_only_say_writing_into_our_own_audio_dir() {
        let dir = Path::new("/tmp/tts-home/audio");
        assert_eq!(orphan_pids(PS, dir, 999, &[]), [101, 103, 107]);
        assert_eq!(orphan_pids(PS, dir, 101, &[]), [103, 107]);
    }

    /// The fix for sentences reaped on a timer: a child this daemon is running
    /// is in our directory by construction, and must still not be selected.
    #[test]
    fn the_reaper_never_selects_a_child_this_daemon_owns() {
        assert_eq!(orphan_pids(PS, Path::new("/tmp/tts-home/audio"), 999, &[101, 103]), [107]);
    }
}
