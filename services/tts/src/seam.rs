//! tts as a cauldron SERVICE: its seam, and the warnings shape it owes.
//!
//! The substrate daemon was a binary cauldron HOSTED: forked, watched, and
//! written a seam FOR (`registers = false`), so its row could never carry a
//! boot token, its probe could never be stronger than a bind check, and the
//! warnings aggregate named it as a gap. This module closes all three.
//!
//! * **The seam.** A second listener on `$CAULDRON_BIND` (`127.0.0.1:0`)
//!   speaks cauldron's line protocol. `HELLO` and `WARNINGS` are answered by
//!   `env::warnings::dispatch`; tts adds `WS`, which answers the websocket
//!   address — how anything in a non-`prod` environment finds a daemon whose
//!   port the kernel chose. The websocket wire itself is untouched.
//! * **The registration.** Written by the service, from inside itself, after
//!   BOTH binds, through `ServiceEnv::register` — which takes an attestation
//!   only [`attest_warnings!`](env::warnings::attest_warnings) can mint, and
//!   that only for a type implementing `ServesWarnings`. Delete the impl below
//!   and this crate does not build.
//! * **The warnings.** Every `tracing` event at WARN or ERROR is counted by
//!   CALL SITE (`file`, `line`) — one defect with eighteen message strings is
//!   one kind, not eighteen — warn and error apart, since this process
//!   started, with a durable record at `<service dir>/warnings/warnings.jsonl`
//!   whose write failures are a counted field, never a log line.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use env::seams::ServiceEnv;
use env::warnings::{Answered, Conn, Export, Kind, Service, ServesWarnings, Verb};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::Layer;

// ═════════════════════════════════════════════════════════ the counts ═════

/// How many call sites an export carries.
pub const TOP_KINDS: usize = 5;
const SINK_FILE: &str = "warnings.jsonl";

static WARN: AtomicU64 = AtomicU64::new(0);
static ERROR: AtomicU64 = AtomicU64::new(0);

struct Site {
    count: u64,
    level: &'static str,
    /// The first message this site wrote, truncated, as its label.
    message: String,
}

struct Ledger {
    kinds: BTreeMap<(String, u32), Site>,
    sink: Option<PathBuf>,
    sink_errors: u64,
}

static LEDGER: Mutex<Ledger> = Mutex::new(Ledger { kinds: BTreeMap::new(), sink: None, sink_errors: 0 });

/// Attach the durable record. Lazy: nothing is opened until the first warning.
pub fn attach_sink(warnings_dir: &Path) {
    if let Ok(mut l) = LEDGER.lock() {
        l.sink = Some(warnings_dir.join(SINK_FILE));
    }
}

/// Count one event at a call site, and append it to the durable record.
pub fn record(level: &'static str, file: &str, line: u32, message: &str) {
    match level {
        "error" => ERROR.fetch_add(1, Ordering::Relaxed),
        _ => WARN.fetch_add(1, Ordering::Relaxed),
    };
    let Ok(mut l) = LEDGER.lock() else { return };
    l.kinds
        .entry((file.to_string(), line))
        .or_insert_with(|| Site { count: 0, level, message: message.chars().take(160).collect() })
        .count += 1;
    let Some(path) = l.sink.clone() else { return };
    let rec = serde_json::json!({"at": now(), "level": level, "target": file, "line": line, "message": message});
    let wrote = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&path)?, "{rec}")
    })();
    // COUNTED, NEVER LOGGED: a failed write to the warnings record reported
    // through the warning path is the original defect with recursion added.
    if wrote.is_err() {
        l.sink_errors += 1;
    }
}

/// The export, as the contract crate's own type.
pub fn export(producer: &str, shape: &str, counts_since: &str) -> Export {
    let (mut kinds, sink_errors, sink_attached) = match LEDGER.lock() {
        Ok(l) => (
            l.kinds
                .iter()
                .map(|((target, line), s)| Kind {
                    target: target.clone(),
                    line: *line,
                    level: s.level.into(),
                    count: s.count,
                    message: s.message.clone(),
                })
                .collect::<Vec<_>>(),
            l.sink_errors,
            l.sink.is_some(),
        ),
        // A panic while counting: the atomics are still true; invent no
        // breakdown.
        Err(_) => (Vec::new(), 0, false),
    };
    kinds.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.target.cmp(&b.target)).then(a.line.cmp(&b.line)));
    kinds.truncate(TOP_KINDS);
    Export {
        shape: shape.into(),
        shape_version: env::warnings::SHAPE_VERSION,
        producer: producer.into(),
        counts_since: counts_since.into(),
        warn: WARN.load(Ordering::Relaxed),
        error: ERROR.load(Ordering::Relaxed),
        sink_errors,
        sink_attached,
        kinds,
    }
}

/// The `tracing` layer that feeds [`record`]: every WARN and ERROR event in
/// the process, keyed by the call site the macro captured.
pub struct WarningsLayer;

impl<S: Subscriber> Layer<S> for WarningsLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        let meta = event.metadata();
        let level = match *meta.level() {
            Level::ERROR => "error",
            Level::WARN => "warn",
            _ => return,
        };
        let mut msg = Message(String::new());
        event.record(&mut msg);
        record(level, meta.file().unwrap_or(meta.target()), meta.line().unwrap_or(0), &msg.0);
    }
}

struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ═════════════════════════════════════════════════════════ the seam ═══════

/// One running instance, as cauldron's line protocol sees it.
pub struct TtsSeam {
    se: ServiceEnv,
    addr: SocketAddr,
    ws: SocketAddr,
    /// THE COUNTING WINDOW: this process's start, never the shape's epoch, so
    /// a restart reads as a restart and not as a drop to zero.
    started_at: String,
}

/// **THE OBLIGATION, DISCHARGED.** Without this impl `attest_warnings!` does
/// not type-check, `register` cannot be called, and the crate does not build.
impl ServesWarnings for TtsSeam {
    const SHAPE: &'static str = env::warnings::SHAPE;

    fn warnings(&self, asked: &str) -> Result<Export> {
        if !asked.is_empty() && asked != Self::SHAPE {
            bail!("this service serves the `{}` shape, not `{asked}`", Self::SHAPE);
        }
        Ok(export(&self.se.service, Self::SHAPE, &self.started_at))
    }
}

/// The service's own words. `HELLO` and `WARNINGS` are cauldron's and cannot
/// be listed here — `attest_warnings!` refuses a table that shadows them.
impl Service for TtsSeam {
    const VERBS: &'static [Verb<TtsSeam>] = &[Verb::new("WS", TtsSeam::v_ws), Verb::new("PING", TtsSeam::v_ping)];

    fn service_env(&self) -> &ServiceEnv {
        &self.se
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl TtsSeam {
    /// `WS` answers `ws://<addr>/ws`: where the websocket actually is.
    fn v_ws(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&format!("ws://{}/ws", self.ws))?;
        Ok(())
    }

    fn v_ping(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line("PONG")?;
        Ok(())
    }
}

/// Bind `$CAULDRON_BIND`, serve the line protocol on a thread per connection,
/// and register the seam. Called only once the websocket is listening, so a
/// row can never claim a daemon nobody can reach.
pub fn start(se: ServiceEnv, ws: SocketAddr, started_at: String) -> Result<SocketAddr> {
    let listener = TcpListener::bind(&se.bind).with_context(|| format!("binding the cauldron seam on {}", se.bind))?;
    let addr = listener.local_addr()?;
    let seam: &'static TtsSeam = Box::leak(Box::new(TtsSeam { se, addr, ws, started_at }));
    std::thread::Builder::new().name("cauldron-seam".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                if let Err(e) = serve_conn(seam, stream) {
                    tracing::warn!(error = %e, "cauldron seam connection failed");
                }
            });
        }
    })?;
    let attested = env::warnings::attest_warnings!(TtsSeam);
    let binary = std::env::var("CAULDRON_BINARY").unwrap_or_default();
    let probe = std::env::var("CAULDRON_PROBE").unwrap_or_else(|_| "seam".into());
    seam.se.register(&addr, &binary, &probe, &attested)?;
    Ok(addr)
}

fn serve_conn(seam: &TtsSeam, stream: TcpStream) -> Result<()> {
    let mut out = stream.try_clone()?;
    let mut lines = BufReader::new(stream);
    let mut c = Conn { out: &mut out, frames: 0 };
    loop {
        let mut line = String::new();
        if lines.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        if env::warnings::dispatch(seam, verb, rest.trim(), &mut c)? == Answered::NotMine {
            tracing::warn!(verb, "unknown request on the cauldron seam");
            c.line(&format!("ERR unknown request `{verb}`"))?;
        }
        c.out.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counted by call site, warn and error apart, top-N by count — and the
    /// counts are process-global atomics, so this reads deltas.
    #[test]
    fn warnings_are_counted_by_call_site_and_level() {
        let before = export("tts", "warnings", "0");
        for _ in 0..3 {
            record("warn", "src/x.rs", 7, "the same site, whatever the text");
        }
        record("error", "src/y.rs", 9, "an error");
        let after = export("tts", "warnings", "0");
        assert_eq!(after.warn - before.warn, 3);
        assert_eq!(after.error - before.error, 1);
        let x = after.kinds.iter().find(|k| k.target == "src/x.rs" && k.line == 7).expect("the site is listed");
        assert_eq!((x.level.as_str(), x.message.as_str()), ("warn", "the same site, whatever the text"));
        assert!(after.kinds.len() <= TOP_KINDS);
    }
}
