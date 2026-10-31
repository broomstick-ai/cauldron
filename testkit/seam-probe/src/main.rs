//! The reference cauldron service — and the proof fixture.
//!
//! It exists to show the whole contract in one small file:
//!
//! 1. It reads its environment and **refuses to start** if any of it is
//!    missing. There is no default data root, so there is no way for it to run
//!    against the wrong one silently.
//! 2. It binds `127.0.0.1:0` — the kernel picks the port — and only then
//!    writes the **real** bound address into the seam table. The file can
//!    therefore only ever claim an address something actually listened on.
//! 3. It writes one line into its own data directory per boot, which is what
//!    makes "two environments are isolated" something you can read off the
//!    disk rather than take on trust.
//! 4. It answers `HELLO` with its identity, including the boot token it was
//!    handed, which is what makes a stale seam detectable.
//! 5. **It serves a warnings shape**, because that is now part of what it means
//!    to be a cauldron service. See the `warnings` module below.
//!
//! Run it by hand with no environment and it exits 2 with the reason. That is
//! the guard, not an inconvenience.
//!
//! # The two halves of its socket, and why they are visibly separate
//!
//! This service serves two completely different kinds of request:
//!
//! * **`WARNINGS`** — a count. Tiny, bounded, and the only payload the
//!   supervisor ever reads.
//! * **`DATA`** — a data frame. What a real consumer is here for, and what the
//!   supervisor must never ask for.
//!
//! The data half exists in this fixture **so that "the supervisor is not in the
//! data path" is a claim that can be false.** If this service served nothing
//! but warnings, the assertion would hold vacuously and would keep holding on
//! the day somebody added a relay. Because data frames are real and are served
//! on the same socket, the assertion is about *restraint*, which is a property
//! that can be violated and therefore a property worth proving.
//!
//! To make that checkable from outside the process, every connection is
//! recorded in [`SERVED_FILE`] with its verb sequence and its data-frame count.
//! `scripts/prove-warnings` partitions those by whether the connection's FIRST
//! verb was one the supervisor is allowed to say
//! ([`env::warnings::SupervisorAsk`]) and asserts that no such connection ever
//! received a frame.
//!
//! # The fault injectors
//!
//! Compliance has to be provable in BOTH directions, so this one binary can
//! also be a non-compliant service. Four switches, all read from the
//! environment (a declaration's `[env]` block sets them), and all absent in the
//! shipped `echo`/`tally` declarations:
//!
//! | variable | what it makes this service |
//! |---|---|
//! | `SEAM_PROBE_NO_WARNINGS=1` | a service that never heard of the contract |
//! | `SEAM_PROBE_WARNINGS_SHAPE=<n>` | a service serving its own shape name |
//! | `SEAM_PROBE_WARNINGS_LIE=shape` | one that answers a shape it was not asked for |
//! | `SEAM_PROBE_WARNINGS_LIE=producer` | one that answers in another service's name |
//! | `SEAM_PROBE_WARNINGS_LIE=counts_since` | one whose count has no window |

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use env::seams::ServiceEnv;
use env::warnings::{Answered, Conn, Export, Kind, Service, ServesWarnings, SupervisorAsk, Verb};

// ═════════════════════════════════════════════════════════════════════════
//  THE WARNINGS SHAPE
// ═════════════════════════════════════════════════════════════════════════

/// # This service counts its own warnings, by CALL SITE, and serves the count
///
/// Deliberately the same design as the reference implementation
/// (`substrate-interface-layer/lib/threads/src/{warnings.rs,shapes/warnings.rs}`),
/// re-derived here only as far as a fixture needs, so the contract has
/// something honest to pass. The five decisions that were carried over rather
/// than re-litigated:
///
/// 1. **A kind is `(file, line)`, not a message string.** Measured in the live
///    daemon: one escalating `error!` appeared under 18 distinct message
///    strings. Keyed by text that is 18 kinds and a top-N that is all one
///    defect; keyed by call site it is one kind with a count of 53.
/// 2. **`warn` and `error` are counted apart and never summed here.** Summing
///    in the producer destroys the distinction with no way back. A face with
///    room for one number sums it itself.
/// 3. **`counts_since` is not optional.** It is this process's start. Without
///    it a restart is indistinguishable from a count falling to zero, and a
///    count that silently resets is the exact lie this feature exists to stop.
/// 4. **A durable record beside the socket, and its failures are a FIELD.** A
///    failed write to the warnings file is reported as `sink_errors` on the
///    shape and is never logged — logging a logging failure recurses.
/// 5. **A `Register`, not a reset-on-idle counter.** Here that is trivially
///    true (these are process statics and nothing can reset them), but it is
///    the reason the totals do not live next to the socket state.
mod warnings {
    use super::*;

    /// The shape name every cauldron service serves unless it declares
    /// another. Taken from the contract crate so the two cannot drift.
    pub fn served_shape() -> String {
        std::env::var("SEAM_PROBE_WARNINGS_SHAPE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| env::warnings::SHAPE.to_string())
    }

    /// The directory the durable record goes in, under the service's OWN data
    /// directory: `<CAULDRON_SERVICE_DATA_DIR>/warnings/warnings.jsonl`.
    ///
    /// **Never `$HOME/.cauldron` and never a path of this service's choosing.**
    /// The only root it knows is the one `cauldron start` handed it, which is
    /// the same guard that keeps it out of production. Under the data-root
    /// layout this becomes `data/<env>/<service>/warnings/`, which is the
    /// `<kind>` directory the layout wants; under the current layout it is
    /// `data/<env>/<service>/warnings/`. The service does not know or
    /// care which, because it only ever joins onto
    /// `$CAULDRON_SERVICE_DATA_DIR`.
    pub const SINK_DIR: &str = "warnings";
    pub const SINK_FILE: &str = "warnings.jsonl";

    /// How many call sites the export carries.
    pub const TOP_KINDS: usize = 5;

    static WARN: AtomicU64 = AtomicU64::new(0);
    static ERROR: AtomicU64 = AtomicU64::new(0);

    pub struct Row {
        pub count: u64,
        pub level: &'static str,
        pub message: String,
    }

    pub struct State {
        pub kinds: BTreeMap<(&'static str, u32), Row>,
        pub sink: Option<std::path::PathBuf>,
        pub sink_errors: u64,
    }

    pub static STATE: Mutex<State> =
        Mutex::new(State { kinds: BTreeMap::new(), sink: None, sink_errors: 0 });

    /// Attach the durable record. Lazy: no file is opened until the first
    /// warning, so a service that never warns writes nothing.
    pub fn attach(dir: &std::path::Path) {
        if let Ok(mut st) = STATE.lock() {
            st.sink = Some(dir.join(SINK_DIR).join(SINK_FILE));
        }
    }

    /// Record one event at a call site. Called only through [`warn_at`] and
    /// [`error_at`], which capture `file!()`/`line!()` so the kind is the call
    /// site and not whatever the caller passed.
    pub fn record(level: &'static str, file: &'static str, line: u32, message: &str) {
        match level {
            "error" => ERROR.fetch_add(1, Ordering::Relaxed),
            _ => WARN.fetch_add(1, Ordering::Relaxed),
        };
        let Ok(mut st) = STATE.lock() else { return };
        let e = st.kinds.entry((file, line)).or_insert_with(|| Row {
            count: 0,
            level,
            // The FIRST message this call site wrote, as its label, truncated:
            // one warning with a 4 KB payload must not set the size of every
            // export.
            message: message.chars().take(160).collect(),
        });
        e.count += 1;
        let Some(path) = st.sink.clone() else { return };
        // THE DURABLE HALF. Its failures are counted, NEVER logged: a failed
        // write to the warnings record, reported through the warning path, is
        // the original defect with recursion added.
        let rec = serde_json::json!({
            "at": now(),
            "level": level,
            "target": file,
            "line": line,
            "message": message,
        });
        let wrote = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut fh = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            fh.write_all(format!("{rec}\n").as_bytes())
        })();
        if wrote.is_err() {
            st.sink_errors += 1;
        }
    }

    /// The export, as the CONTRACT CRATE's own type.
    ///
    /// It used to be a hand-built `serde_json::Value`. It is
    /// [`env::warnings::Export`] now because `ServesWarnings::warnings` returns
    /// that type: producer and consumer share one definition, so a field the
    /// supervisor checks cannot be missing from what a producer builds.
    pub fn export(se: &ServiceEnv, started_at: &str) -> Export {
        let warn = WARN.load(Ordering::Relaxed);
        let error = ERROR.load(Ordering::Relaxed);
        let (mut kinds, sink_errors, attached) = match STATE.lock() {
            Ok(st) => {
                let k: Vec<Kind> = st
                    .kinds
                    .iter()
                    .map(|(&(file, line), r)| Kind {
                        target: file.to_string(),
                        line,
                        level: r.level.to_string(),
                        count: r.count,
                        message: r.message.clone(),
                    })
                    .collect();
                (k, st.sink_errors, st.sink.is_some())
            }
            // A poisoned lock means a panic happened while counting. The totals
            // are atomics and still true, so report them and report an EMPTY
            // breakdown rather than inventing one.
            Err(_) => (Vec::new(), 0, false),
        };
        kinds.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.target.cmp(&b.target))
                .then_with(|| a.line.cmp(&b.line))
        });
        kinds.truncate(TOP_KINDS);

        let lie = std::env::var("SEAM_PROBE_WARNINGS_LIE").unwrap_or_default();
        Export {
            shape: if lie == "shape" { "gossip".to_string() } else { served_shape() },
            shape_version: env::warnings::SHAPE_VERSION,
            producer: if lie == "producer" {
                "somebody-else".to_string()
            } else {
                se.service.clone()
            },
            counts_since: if lie == "counts_since" {
                String::new()
            } else {
                started_at.to_string()
            },
            warn,
            error,
            sink_errors,
            sink_attached: attached,
            kinds,
        }
    }
}

/// Count one warning, keyed by THIS call site.
macro_rules! warn_at {
    ($($arg:tt)*) => {
        warnings::record("warn", file!(), line!(), &format!($($arg)*))
    };
}

/// Count one error, keyed by THIS call site. Errors are counted apart from
/// warnings and never folded into them.
macro_rules! error_at {
    ($($arg:tt)*) => {
        warnings::record("error", file!(), line!(), &format!($($arg)*))
    };
}

// ═════════════════════════════════════════════════════════════════════════
//  THE CONNECTION RECORD — how "not in the data path" is checked from outside
// ═════════════════════════════════════════════════════════════════════════

/// `<data_dir>/served.jsonl` — one line per connection, written when it closes.
///
/// This is the producer's own account of who asked it for what. The proof reads
/// it; nothing in the supervisor does, and nothing in the service's behaviour
/// depends on it.
const SERVED_FILE: &str = "served.jsonl";

static CONNS: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ═════════════════════════════════════════════════════════════════════════
//  THE SERVICE ITSELF — and the COMPILE-TIME half of the contract
// ═════════════════════════════════════════════════════════════════════════

/// Everything one running instance is.
///
/// It exists as a type because the warnings obligation is a TRAIT now: there
/// has to be something for `impl ServesWarnings for _` to name, and that is the
/// whole mechanism. Before this, `serve()` was a free function over a `match`
/// and there was nothing a compiler could hold responsible.
struct Probe {
    se: ServiceEnv,
    addr: std::net::SocketAddr,
    /// THE COUNTING WINDOW. This process's start, carried on every export as
    /// `counts_since`.
    started_at: String,
}

/// **THE OBLIGATION, DISCHARGED.** Delete this impl and the binary does not
/// build; the error names both items it wants. `scripts/prove-compile-gate`
/// deletes it on purpose and prints what rustc says.
impl ServesWarnings for Probe {
    /// The shape this service declares at COMPILE TIME, taken from the contract
    /// crate so the two cannot drift. The shape it ANSWERS with is
    /// [`warnings::served_shape`] and is deliberately allowed to differ: a
    /// service may make it configurable, and whether the answer matches what
    /// the declaration asked for is the supervisor's question, not the
    /// compiler's. (`SEAM_PROBE_WARNINGS_SHAPE` is this fixture exercising
    /// exactly that.)
    const SHAPE: &'static str = env::warnings::SHAPE;

    /// Three refusals and one answer, and every one of them is a RUNTIME fact —
    /// which is the honest division of labour. The compiler proved this function
    /// exists and is reached; only a socket can prove what it says.
    fn warnings(&self, asked: &str) -> Result<Export> {
        if std::env::var("SEAM_PROBE_NO_WARNINGS").is_ok() {
            // A service that never heard of the contract. It answers the way any
            // service does to a verb it does not know, which is what makes the
            // refusal path realistic rather than a special "I refuse" reply the
            // supervisor could be taught to recognise.
            bail!("unknown request `WARNINGS`");
        }
        let served = warnings::served_shape();
        if !asked.is_empty() && asked != served {
            bail!("this service serves the `{served}` shape, not `{asked}`");
        }
        Ok(warnings::export(&self.se, &self.started_at))
    }
}

/// **THE SERVICE'S OWN HALF OF THE SOCKET.** `HELLO` and `WARNINGS` are absent
/// from this table on purpose and cannot be added to it: they are the
/// supervisor's two words, cauldron answers them in `env::warnings::dispatch`,
/// and `attest_warnings!` refuses at compile time to let a service shadow one.
/// `scripts/prove-compile-gate` adds `WARNINGS` here on purpose to show it.
impl Service for Probe {
    const VERBS: &'static [Verb<Probe>] = &[
        Verb::new("WHOAMI", Probe::v_whoami),
        Verb::new("DATA", Probe::v_data),
        Verb::new("WARN", Probe::v_warn),
        Verb::new("ERROR", Probe::v_error),
        Verb::new("PING", Probe::v_ping),
        Verb::new("ENV", Probe::v_env),
    ];

    fn service_env(&self) -> &ServiceEnv {
        &self.se
    }
    fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }
}

impl Probe {
    /// A local alias for the identity handshake. `HELLO` itself belongs to
    /// cauldron; this is the same answer under a name a human types.
    fn v_whoami(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&serde_json::to_string(&self.se.identity(&self.addr))?)?;
        Ok(())
    }

    /// **THE DATA PATH. The supervisor must never reach this handler.**
    ///
    /// `DATA [n]` answers one frame of about `n` bytes. It exists so that "no
    /// data frame crossed the supervisor" is a statement about a path that is
    /// real and in use, and therefore a statement that can be false.
    fn v_data(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        let n: usize = rest.parse().unwrap_or(256);
        let n = n.min(64 * 1024);
        c.frames += 1;
        let frame = serde_json::json!({
            "frame": c.frames,
            "service": self.se.service,
            "payload": "x".repeat(n),
        });
        c.line(&frame.to_string())?;
        Ok(())
    }

    /// ONE fixed call site, so `WARN a` ten times is ONE kind with a count of
    /// ten — the whole argument for keying a kind by call site rather than by
    /// message text, demonstrated by the fixture that proves it.
    fn v_warn(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        warn_at!("{}", if rest.is_empty() { "asked to warn" } else { rest });
        c.line("OK")?;
        Ok(())
    }

    fn v_error(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        error_at!("{}", if rest.is_empty() { "asked to error" } else { rest });
        c.line("OK")?;
        Ok(())
    }

    fn v_ping(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line("PONG")?;
        Ok(())
    }

    fn v_env(&self, c: &mut Conn<'_>, _rest: &str) -> Result<()> {
        c.line(&format!(
            "{} {} {}",
            self.se.service,
            self.se.env,
            self.se.data_root.display()
        ))?;
        Ok(())
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("seam-probe: {e:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let se = ServiceEnv::from_env()?;
    let listener = TcpListener::bind(&se.bind).with_context(|| format!("binding {}", se.bind))?;
    let addr = listener.local_addr()?;

    std::fs::create_dir_all(&se.data_dir)?;
    // THE COUNTING WINDOW. This process's start, carried on every export as
    // `counts_since`, so a restart reads as a restart and not as a drop to
    // zero. Seconds since the epoch: a number, because every reader of it is
    // comparing it, not printing it in a sentence.
    let started_at = now().to_string();
    warnings::attach(&se.data_dir);

    // Per-boot evidence, inside this environment's data root and nowhere else.
    let marker = se.data_dir.join("boots.log");
    {
        // ONE `write_all` of a pre-formatted line. `writeln!` with format
        // arguments issues several `write` calls, and two processes appending
        // to one shared `boots.log` (exactly the sharing the environments
        // test forces) then tear each other's lines: `service=service=echo
        // env=alpha … / echo env=beta …`. One write on an `O_APPEND` fd lands
        // whole, so a shared file shows two whole lines.
        let line = format!(
            "service={} env={} data_root={} addr={} pid={} version={} token={}\n",
            se.service,
            se.env,
            se.data_root.display(),
            addr,
            std::process::id(),
            se.version,
            se.boot_token
        );
        let mut fh = std::fs::OpenOptions::new().create(true).append(true).open(&marker)?;
        fh.write_all(line.as_bytes())?;
    }

    let probe = Probe { se, addr, started_at };

    // ─────────────────────────────────────────────────────────────────────
    //  THE COMPILE-TIME CHECK, at the only place it could be: the act of
    //  writing a seam.
    //
    //  `register` takes an `Attestation`, and this macro is the only thing
    //  that mints one. It does not type-check unless `Probe` implements
    //  `ServesWarnings`, and it const-asserts that the declared shape name is
    //  usable and that `Service::VERBS` does not shadow `WARNINGS`.
    //
    //  So a non-compliant producer never reaches a socket, never writes an
    //  address, and is not something `cauldron start` has to refuse — it is
    //  something `cargo build` refuses.
    // ─────────────────────────────────────────────────────────────────────
    let attested = env::warnings::attest_warnings!(Probe);

    let binary = std::env::var("CAULDRON_BINARY").unwrap_or_default();
    let probe_kind = std::env::var("CAULDRON_PROBE").unwrap_or_else(|_| "seam".into());
    probe.se.register(&addr, &binary, &probe_kind, &attested)?;

    println!(
        "seam-probe: service={} env={} data_root={} listening on {} (pid {}, version {}), \
         warnings shape `{}` (attested at compile time as `{}`){}",
        probe.se.service,
        probe.se.env,
        probe.se.data_root.display(),
        addr,
        std::process::id(),
        probe.se.version,
        warnings::served_shape(),
        attested.shape(),
        if std::env::var("SEAM_PROBE_NO_WARNINGS").is_ok() {
            " -- SUPPRESSED by SEAM_PROBE_NO_WARNINGS"
        } else {
            ""
        }
    );
    let _ = std::io::stdout().flush();

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if let Err(e) = serve(&probe, stream) {
            // A connection that fell over is a real warning, at its own call
            // site, and it is now something a consumer can see.
            warn_at!("connection failed: {e:#}");
            eprintln!("seam-probe: connection: {e:#}");
        }
    }
    Ok(())
}

/// Serve one connection until the peer closes it.
///
/// **A LOOP, not a single line, deliberately.** A connection is a verb
/// SEQUENCE, and the data-path assertion is about sequences: the thing it has
/// to be able to catch is a connection that opens with the supervisor's
/// handshake and then asks for a frame. A service that hung up after one line
/// could not express that, so the assertion would be unfalsifiable.
///
/// **The routing is `env::warnings::dispatch`, not a `match` here.** That is
/// what makes the compile-time check about more than a decorative impl: the
/// `WARNINGS` word is answered by the contract crate's own code, which calls
/// `ServesWarnings::warnings`, so the impl the compiler demanded is the impl
/// that actually runs.
fn serve(p: &Probe, stream: TcpStream) -> Result<()> {
    let conn = CONNS.fetch_add(1, Ordering::Relaxed);
    let peer = stream.peer_addr().map(|q| q.to_string()).unwrap_or_default();
    let mut w = stream.try_clone()?;
    let mut r = BufReader::new(stream);
    let mut verbs: Vec<String> = Vec::new();
    let mut c = Conn { out: &mut w, frames: 0 };

    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let mut it = line.splitn(2, ' ');
        let verb = it.next().unwrap_or("").to_string();
        let rest = it.next().unwrap_or("").trim().to_string();
        verbs.push(verb.clone());

        match env::warnings::dispatch(p, &verb, &rest, &mut c)? {
            Answered::Yes => {}
            Answered::NotMine => {
                // An unknown request is a real warning at a real call site.
                // This is the one that moves on its own in normal operation,
                // which is how the aggregate has something true to show without
                // anybody staging it.
                warn_at!("unknown request `{verb}`");
                c.line(&format!("ERR unknown request `{verb}`"))?;
            }
        }
        c.out.flush()?;
    }
    let data_frames = c.frames;

    // The connection's own record. `supervisor_shaped` is decided by the
    // CONTRACT CRATE's closed vocabulary, not by a copy of it here, so adding a
    // third supervisor verb changes this classification and the proof's
    // partition in the same edit.
    let first = verbs.first().cloned().unwrap_or_default();
    let rec = serde_json::json!({
        "conn": conn,
        "at": now(),
        "peer": peer,
        "first": first,
        "supervisor_shaped": SupervisorAsk::is_supervisor_word(&first),
        "verbs": verbs,
        "data_frames": data_frames,
    });
    if let Ok(mut fh) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p.se.data_dir.join(SERVED_FILE))
    {
        let _ = fh.write_all(format!("{rec}\n").as_bytes());
    }
    Ok(())
}
