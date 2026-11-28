//! **Dumps**: raw text put into a granted cauldron for later processing,
//! kept in the cauldron's dumps store beside its soup
//! (`soup_door::dumps`), never in the soup.
//!
//! # The store, held
//!
//! The first dump-scoped write to a cauldron opens its dumps store (making
//! it, the first time anything is dumped there): the store's writer lock is
//! taken exclusive and `mcp <pid>` written into it, then its spine is opened,
//! which recovers a dump that a killed service left half done (rolled back
//! before its decision, landed after it). The store is held, like the soup's
//! writer, until the grant loses `dump` or is revoked, or the service stops;
//! there is no idle close. While it is held, `cauldron soup dump` sends its
//! text here.
//!
//! # One dump, one tranche
//!
//! A dump's bytes go to the blob store first, as a temporary file. Then,
//! holding the store, the grant is read again (a revoke that came while the
//! bytes arrived refuses the dump), the bytes are synced and named by their
//! SHA-256, and ONE transaction creates `DM-<n>` in state `received`. A
//! dump refused or failed at any of these steps leaves no byte behind: the
//! temporary file is removed, and so is a named blob no dump refers to. A
//! transcript still arriving when its grant is revoked stops being read. Its commit ends the
//! write and the store is let go for the next dump. Only after that is
//! [`SideEffects::after_commit`] called: whatever a dump sets off later runs
//! after its write, never inside it, and the write never waits on it. The
//! side effects are a log line and, when automatic processing is on for the
//! cauldron, a word to the `ripples` service ([`NotifyRipples`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use shapewire::envelope::Refusal;
use soup_door::dumps::{self, By, Landed, NewDump, Store};
use soup_door::Target;

use crate::access::Access;
use crate::audit::{Audit, Line};
use crate::refusal;
use crate::writer::Who;

/// What landed, as a side effect sees it.
#[derive(Debug, Clone)]
pub enum Landing<'a> {
    /// A dump was received.
    Dump { env: &'a str, cauldron: &'a str, id: &'a str, kind: &'a str, bytes: u64, cursor: &'a str },
}

/// **What a landed write sets off**, called after its transaction has
/// committed and the store has been let go. The hook later processing
/// attaches to; nothing it does can undo or delay the write.
pub trait SideEffects: Send + Sync {
    fn after_commit(&self, landed: &Landing<'_>);
}

/// This release's side effects: a log line, and nothing else.
pub struct LogOnly;

impl SideEffects for LogOnly {
    fn after_commit(&self, landed: &Landing<'_>) {
        match landed {
            Landing::Dump { env, cauldron, id, kind, bytes, cursor } => tracing::info!(
                %env, %cauldron, dump = %id, %kind, bytes, %cursor,
                "dump received; it waits as `received` until its owner processes it (`cauldron ripples run`)"
            ),
        }
    }
}

/// **The side effects of a landed dump**: the log line [`LogOnly`] writes,
/// and, when automatic processing is on for the dump's cauldron
/// (`env::ripples_auto`), a `DUMP` line to the `ripples` service's seam, so
/// it looks at that cauldron now rather than at its next scan. The line goes
/// from its own thread, with short timeouts: it never delays or fails the
/// dump, and a line that is lost (the service is not running, or does not
/// answer) costs only the wait until the service's next scan, which finds
/// every dump still `received`. The line carries the service's run key
/// (`<data root>/ripples/run.secret`) and names only the cauldron and the
/// dump; the service reads everything else through the ripple seam.
pub struct NotifyRipples {
    pub env: env::Environment,
}

impl SideEffects for NotifyRipples {
    fn after_commit(&self, landed: &Landing<'_>) {
        LogOnly.after_commit(landed);
        let Landing::Dump { cauldron, id, kind, .. } = landed;
        if !env::ripples_auto::is_on(&self.env, cauldron) {
            return;
        }
        let (e, line) = (self.env.clone(), json!({"cauldron": cauldron, "dump": id, "kind": kind}));
        let _ = std::thread::Builder::new().name("notify-ripples".into()).spawn(move || match notify(&e, &line) {
            Ok(()) => tracing::info!(dump = %line["dump"], "told the ripples service a dump landed"),
            Err(why) => tracing::info!(dump = %line["dump"], %why, "could not tell the ripples service a dump landed; its next scan finds it"),
        });
    }
}

/// One `DUMP` line to the `ripples` seam of `e` (loopback only), and its
/// answer read, all within a few seconds.
fn notify(e: &env::Environment, line: &Value) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    let t = env::seams::Table::load(&e.name, &e.data_root, &e.seams()).map_err(|x| format!("the seam table: {x:#}"))?;
    let row = t.seams.get("ripples").ok_or("the ripples service is not running")?;
    let addr: std::net::SocketAddr = row.addr.parse().map_err(|_| format!("the ripples seam `{}` is not ip:port", row.addr))?;
    if !addr.ip().is_loopback() {
        return Err(format!("the ripples seam `{addr}` is not a loopback address; nothing was sent"));
    }
    let key = std::fs::read_to_string(e.data_root.join("ripples").join("run.secret")).map_err(|x| format!("the ripples run key: {x}"))?;
    let s = env::egress::connect_tcp("ripples seam (dump landed)", &addr, std::time::Duration::from_secs(2)).map_err(|x| x.to_string())?;
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(5)));
    let mut w = s.try_clone().map_err(|x| x.to_string())?;
    w.write_all(format!("DUMP {} {line}\n", key.trim()).as_bytes()).and_then(|_| w.flush()).map_err(|x| x.to_string())?;
    let mut answer = String::new();
    BufReader::new(s).read_line(&mut answer).map_err(|x| x.to_string())?;
    match serde_json::from_str::<Value>(answer.trim()) {
        Ok(v) if v["ok"] == json!(true) => Ok(()),
        _ => Err(format!("the ripples service answered `{}`", answer.trim())),
    }
}

/// One cauldron's dumps store, held open.
pub struct Held {
    pub opened_at: String,
    pub recovered: Value,
    store: Mutex<Option<Store>>,
}

/// Every dumps store this service holds.
pub struct Dumps {
    audit: Arc<Audit>,
    access: Arc<Access>,
    effects: Arc<dyn SideEffects>,
    /// Bumped after every dump write, for a waiting `soup_changes`.
    pulse: Arc<crate::changes::Pulse>,
    open: Mutex<HashMap<String, Arc<Held>>>,
    /// Held while a store is being opened, so opens run one at a time
    /// without holding `open` (see `writer::Writers`).
    opening: Mutex<()>,
}

fn closed() -> Refusal {
    Refusal::new(
        "not_granted",
        "the cauldron's dumps store was closed to this door (its grant was revoked or lost `dump`, or the service is \
         stopping) before this dump was written. Nothing was written.",
    )
}

fn revoked(cauldron: &str, why: &str) -> Refusal {
    Refusal::new(
        "not_granted",
        format!(
            "cauldron `{cauldron}` is closed to this door: {why} before this dump was written. Nothing was written. \
             The owner opens it again from a terminal with `cauldron mcp grant`."
        ),
    )
}

impl Dumps {
    pub fn new(audit: Arc<Audit>, access: Arc<Access>, effects: Arc<dyn SideEffects>, pulse: Arc<crate::changes::Pulse>) -> Dumps {
        Dumps { audit, access, effects, pulse, open: Mutex::new(HashMap::new()), opening: Mutex::new(()) }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Held>>> {
        self.open.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The names of every cauldron whose dumps store this service holds.
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map().keys().cloned().collect();
        v.sort();
        v
    }

    /// Every held dumps store, as `cauldron mcp status` shows it:
    /// `[{cauldron, opened_at, recovered_at_open}]`, by name.
    pub fn snapshot(&self) -> Value {
        let open = self.map();
        let mut v: Vec<(&String, &Arc<Held>)> = open.iter().collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        Value::Array(
            v.into_iter()
                .map(|(name, h)| serde_json::json!({"cauldron": name, "opened_at": h.opened_at, "recovered_at_open": h.recovered}))
                .collect(),
        )
    }

    /// Is `cauldron`'s dumps store held here?
    pub fn holds(&self, cauldron: &str) -> bool {
        self.map().contains_key(cauldron)
    }

    fn get_or_open(&self, t: &Target, who: &Who) -> Result<Arc<Held>, Refusal> {
        if let Some(h) = self.map().get(&t.cauldron.name) {
            return Ok(h.clone());
        }
        let _opening = self.opening.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = self.map().get(&t.cauldron.name) {
            return Ok(h.clone());
        }
        if let Some(why) = self.access.why_not_dumpable(&t.cauldron) {
            return Err(revoked(&t.cauldron.name, why));
        }
        let store = Store::open(&t.cauldron, &format!("{} {}", soup_door::lock::MCP_SERVICE, std::process::id())).map_err(refusal)?;
        let h = Arc::new(Held {
            opened_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            recovered: store.recovered.clone(),
            store: Mutex::new(Some(store)),
        });
        self.map().insert(t.cauldron.name.clone(), h.clone());
        tracing::info!(cauldron = %t.cauldron.name, "opened a cauldron's dumps store");
        self.audit.write(Line {
            client: Some(who.client.clone()),
            session: who.session.clone(),
            cauldron: Some(t.cauldron.name.clone()),
            tool: "open-dumps".into(),
            outcome: "opened".into(),
            recovered: Some(h.recovered.clone()),
            ..Line::default()
        });
        Ok(h)
    }

    /// **Run `f` with the held store of `cauldron`**, mutably, when this
    /// service holds it (`None` when it does not, or it is closing).
    pub fn with_held_mut<R>(&self, cauldron: &str, f: impl FnOnce(&mut Store) -> R) -> Option<R> {
        let h = self.map().get(cauldron).cloned()?;
        let mut g = h.store.lock().unwrap_or_else(|p| p.into_inner());
        g.as_mut().map(f)
    }

    /// Open (or find) the store, for a dump about to be written: the blob
    /// store is then ready to take its bytes.
    pub fn prepare(&self, t: &Target, who: &Who) -> Result<(), Refusal> {
        self.get_or_open(t, who).map(|_| ())
    }

    /// Is a dump into `t` still allowed (its grant read again)?
    pub fn still_allowed(&self, t: &Target) -> Result<(), Refusal> {
        match self.access.why_not_dumpable(&t.cauldron) {
            Some(why) => Err(revoked(&t.cauldron.name, why)),
            None => Ok(()),
        }
    }

    /// **Land a dump** whose bytes are in `blob` (not yet named): hold the
    /// store, read the grant again, name the blob, build the dump with
    /// `make` (given the blob and its path), write ONE transaction, let the
    /// store go, then call the side effects. Answers the receipt. A refusal
    /// or an error at any step leaves no byte: the temporary file, or the
    /// named blob no dump refers to, is removed (`Store::commit`).
    pub fn commit(
        &self,
        t: &Target,
        who: &Who,
        blob: dumps::BlobWriter,
        make: impl FnOnce(&dumps::Blob, &std::path::Path) -> Result<NewDump, Refusal>,
        dispatch: &str,
    ) -> Result<Value, Refusal> {
        let h = self.get_or_open(t, who)?;
        let (dump, landed): (NewDump, Landed) = {
            let store = h.store.lock().unwrap_or_else(|p| p.into_inner());
            let Some(store) = store.as_ref() else { return Err(closed()) };
            let by = By {
                thread: who.thread.clone(),
                dispatch: dispatch.to_string(),
                instrument: "cauldron-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            };
            let out = store.commit(blob, || self.still_allowed(t), |b| make(b, &store.blob_path(b)), &by, refusal);
            // A waiting `soup_changes` looks again, whatever came of it.
            self.pulse.bump();
            out?
        };
        let cursor = crate::cursor::now(&t.cauldron).unwrap_or_default();
        let mut receipt = dumps::receipt(&t.cauldron.name, &t.env.name, &dump, &landed);
        receipt["cursor"] = json!(cursor);
        // Whether automatic processing takes this dump (`env::ripples_auto`).
        let mut auto = env::ripples_auto::receipt(&t.env, &t.cauldron.name, &landed.id, dump.kind().as_str());
        if auto["takes_this_dump"] == json!(true) {
            let registered = env::seams::Table::load(&t.env.name, &t.env.data_root, &t.env.seams()).map(|x| x.seams.contains_key("ripples")).unwrap_or(false);
            auto["service"] = json!(if registered {
                "registered in the seam table".to_string()
            } else {
                format!("not running: nothing is processed until it is started (`cauldron --env {} start ripples`)", t.env.name)
            });
        }
        receipt["automatic"] = auto;
        self.effects.after_commit(&Landing::Dump {
            env: &t.env.name,
            cauldron: &t.cauldron.name,
            id: &landed.id,
            kind: dump.kind().as_str(),
            bytes: dump.blob.bytes,
            cursor: &cursor,
        });
        Ok(receipt)
    }

    /// Run `f` on `t`'s dumps store, holding it (opened first if needed, as
    /// for a dump).
    pub fn with_store<R>(&self, t: &Target, who: &Who, f: impl FnOnce(&Store) -> Result<R, Refusal>) -> Result<R, Refusal> {
        let h = self.get_or_open(t, who)?;
        let store = h.store.lock().unwrap_or_else(|p| p.into_inner());
        let Some(store) = store.as_ref() else { return Err(closed()) };
        let out = f(store);
        self.pulse.bump();
        out
    }

    /// Close `cauldron`'s dumps store: the dump being written (if any)
    /// finishes, then the store and its lock are let go. A no-op when it is
    /// not held.
    pub fn close(&self, cauldron: &str, reason: &str) {
        let Some(h) = self.map().remove(cauldron) else { return };
        h.store.lock().unwrap_or_else(|p| p.into_inner()).take();
        tracing::info!(%cauldron, %reason, "closed a cauldron's dumps store; its lock is free");
        self.audit.write(Line {
            cauldron: Some(cauldron.to_string()),
            tool: "close-dumps".into(),
            outcome: "closed".into(),
            reason: Some(reason.to_string()),
            ..Line::default()
        });
    }

    /// Close every held store.
    pub fn close_all(&self, reason: &str) {
        for n in self.names() {
            self.close(&n, reason);
        }
    }
}
