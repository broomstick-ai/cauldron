//! **The cauldron seam a producer registers through** (H179-1: cauldron hosts
//! every service, and a service becomes visible to it by registering).
//!
//! Three things, each one a rule a producer would otherwise re-derive:
//!
//! * **The fixed-port rule** ([`port`], [`listen`]) — R3.S2.0's decision 2,
//!   which that step designed and did not write. A service whose port the
//!   operator's faces dial (threads `:3653`, tts `:3652`) states it; an
//!   unstated port is EPHEMERAL, except in [`FACES_ENV`], where it is REFUSED:
//!   the faces dial a fixed port there and read no seam, so a `prod` daemon on
//!   a kernel-chosen port is a healthy process nobody can reach. The primitive
//!   takes the CONFIGURED VALUE; it does not go looking for it, so this crate
//!   carries no file format (`Cargo.toml`, "NO `toml`").
//! * **The line protocol** ([`serve`]) — a listener on `$CAULDRON_BIND` that
//!   answers cauldron's two words through `env::warnings::dispatch` and the
//!   service's own `Service::VERBS` after them. Lifted from
//!   `services/tts/src/seam.rs`, made generic over the service.
//! * **The registration** ([`start`]) — bind, serve, then
//!   `ServiceEnv::register`, which takes the `Attestation` only
//!   `env::warnings::attest_warnings!` can mint. The CALLER mints it: the
//!   macro's two `const` assertions cannot be written over a generic
//!   parameter, so [`start`] takes the attestation and checks at runtime that
//!   it was minted for the type it is serving ([`SeamError::WrongAttestation`]).
//!
//! The door ([`crate::server`]) is not here and does not know this exists:
//! `seam` is its own feature, and `--features server` cannot reach `env`.
//!
//! # Errors are local
//!
//! `env` returns `anyhow`; this crate names no `anyhow` (`check-features.sh`
//! asserts it). Every `env` failure is carried as [`SeamError::Env`] with its
//! full chain rendered, which is all a caller does with it anyway.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use env::seams::ServiceEnv;
use env::warnings::{Answered, Attestation, Conn, ServesWarnings, Service};

/// The one environment whose faces dial fixed ports and read no seam.
pub const FACES_ENV: &str = "prod";

/// What can go wrong at the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeamError {
    /// [`FACES_ENV`] with no stated port. Carries the environment.
    PortRequired { env: String },
    /// A bind that failed: the address tried, and the OS's words.
    Bind { addr: String, error: String },
    /// An attestation minted for a different shape than the service serves.
    WrongAttestation { attested: &'static str, serves: &'static str },
    /// `env` refused (the seam table, mostly), its chain rendered.
    Env(String),
    /// I/O on the listener itself.
    Io(String),
}

impl std::fmt::Display for SeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeamError::PortRequired { env } => write!(
                f,
                "REFUSED: environment `{env}` states no port. The faces dial a fixed port there \
                 and read no seam, so an ephemeral port would be a daemon nobody can reach. \
                 State the port in this service's config for `{env}`."
            ),
            SeamError::Bind { addr, error } => write!(f, "binding {addr}: {error}"),
            SeamError::WrongAttestation { attested, serves } => write!(
                f,
                "REFUSED: the attestation was minted for shape `{attested}` but this service \
                 serves `{serves}`. Mint it with `attest_warnings!` for the type being served."
            ),
            SeamError::Env(e) => write!(f, "{e}"),
            SeamError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SeamError {}

impl From<std::io::Error> for SeamError {
    fn from(e: std::io::Error) -> Self {
        SeamError::Io(e.to_string())
    }
}

// ═════════════════════════════════════════════════════ the fixed-port rule ══

/// **THE FIXED-PORT RULE.** A stated port is used as stated. An unstated one is
/// `0` (the kernel's choice), except in [`FACES_ENV`], where it is refused.
pub fn port(configured: Option<u16>, env: &str) -> Result<u16, SeamError> {
    match configured {
        Some(p) => Ok(p),
        None if env == FACES_ENV => Err(SeamError::PortRequired { env: env.to_string() }),
        None => Ok(0),
    }
}

/// Bind `host` on the port [`port`] decides. A std listener: a tokio producer
/// converts it with `tokio::net::TcpListener::from_std` after
/// `set_nonblocking(true)`, so the rule does not depend on a runtime.
pub fn listen(host: &str, configured: Option<u16>, env: &str) -> Result<TcpListener, SeamError> {
    let p = port(configured, env)?;
    let addr = format!("{host}:{p}");
    TcpListener::bind(&addr).map_err(|e| SeamError::Bind { addr, error: e.to_string() })
}

// ═════════════════════════════════════════════════════════ the line protocol ══

/// Serve cauldron's line protocol on `listener`, a thread per connection, until
/// the process exits. Returns once the accept thread is running.
pub fn serve<S: Service + Sync>(svc: &'static S, listener: TcpListener) -> Result<(), SeamError> {
    std::thread::Builder::new().name("cauldron-seam".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                if let Err(e) = serve_conn(svc, stream) {
                    tracing::warn!(error = %e, "cauldron seam connection failed");
                }
            });
        }
    })?;
    Ok(())
}

/// One connection: a line in, `env::warnings::dispatch`, a line out. A word
/// neither cauldron nor the service owns is answered `ERR unknown request`.
pub fn serve_conn<S: Service>(svc: &S, stream: TcpStream) -> Result<(), SeamError> {
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
        let answered =
            env::warnings::dispatch(svc, verb, rest.trim(), &mut c).map_err(|e| SeamError::Env(format!("{e:#}")))?;
        if answered == Answered::NotMine {
            tracing::warn!(verb, "unknown request on the cauldron seam");
            c.line(&format!("ERR unknown request `{verb}`"))?;
        }
        c.out.flush()?;
    }
}

// ═══════════════════════════════════════════════════════════ the registration ══

/// The two row fields `cauldron start` hands a service in its environment that
/// [`ServiceEnv`] does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// `$CAULDRON_BINARY`: the image this process was started from.
    pub binary: String,
    /// `$CAULDRON_PROBE`: how the supervisor checks the row; `seam` (the HELLO
    /// handshake) unless stated.
    pub probe: String,
}

impl Registration {
    /// Read from the process environment, with tts's defaults.
    pub fn from_env() -> Self {
        Registration {
            binary: std::env::var("CAULDRON_BINARY").unwrap_or_default(),
            probe: std::env::var("CAULDRON_PROBE").unwrap_or_else(|_| "seam".into()),
        }
    }
}

/// **BIND, SERVE, REGISTER — in that order**, so a row can never claim a
/// listener that is not there. `make` builds the service once the seam's own
/// address is known (the service answers `HELLO` with it). The service is
/// leaked: it lives as long as the process, which is how long its row does.
///
/// Call it only after the producer's DOOR is listening, so the row can never
/// name a daemon nobody can reach.
pub fn start<S, F>(
    se: &ServiceEnv,
    attested: &Attestation,
    reg: &Registration,
    make: F,
) -> Result<(&'static S, SocketAddr), SeamError>
where
    S: Service + Sync,
    F: FnOnce(SocketAddr) -> S,
{
    let serves = <S as ServesWarnings>::SHAPE;
    if attested.shape() != serves {
        return Err(SeamError::WrongAttestation { attested: attested.shape(), serves });
    }
    let listener =
        TcpListener::bind(&se.bind).map_err(|e| SeamError::Bind { addr: se.bind.clone(), error: e.to_string() })?;
    let addr = listener.local_addr()?;
    let svc: &'static S = Box::leak(Box::new(make(addr)));
    serve(svc, listener)?;
    se.register(&addr, &reg.binary, &reg.probe, attested).map_err(|e| SeamError::Env(format!("{e:#}")))?;
    Ok((svc, addr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::warnings::{Counter, Level};
    use env::seams::{SeamState, Table};
    use env::warnings::{Export, Verb};

    // ── the fixed-port rule ────────────────────────────────────────────────

    /// All three arms, each paired with its opposite, so neither a constant
    /// `Ok(0)` nor a constant refusal passes.
    #[test]
    fn an_unstated_port_is_ephemeral_outside_prod_and_refused_in_it() {
        assert_eq!(port(None, "r8"), Ok(0), "unstated, outside prod: the kernel's choice");
        let e = port(None, FACES_ENV).unwrap_err();
        assert_eq!(e, SeamError::PortRequired { env: "prod".into() });
        assert!(e.to_string().contains("states no port"), "the refusal names the fix: {e}");
        assert_eq!(port(Some(3653), FACES_ENV), Ok(3653), "stated, in prod: as stated");
        assert_eq!(port(Some(3653), "r8"), Ok(3653), "stated, outside prod: as stated");
    }

    /// **THE BOUND PORT EQUALS THE CONFIGURED PORT, by two independent routes**:
    /// the number this test chose (below the ephemeral range, so the kernel
    /// cannot hand it out by accident) against the OS's `local_addr` of the
    /// listener AND a connection the OS routed to that number. A `listen` that
    /// ignored `configured` and bound `:0` gets an ephemeral port (>= 49152 on
    /// this OS) and fails the first equality.
    #[test]
    fn the_bound_port_is_the_configured_port() {
        let (l, chosen) = (20_000u16..29_999)
            .find_map(|p| listen("127.0.0.1", Some(p), "r8").ok().map(|l| (l, p)))
            .expect("a free port below the ephemeral range");
        assert!(chosen < 49_152, "the configured value is not one the kernel would pick");
        assert_eq!(l.local_addr().unwrap().port(), chosen, "route 1: the OS says where it bound");
        let s = TcpStream::connect(("127.0.0.1", chosen)).expect("route 2: a connection to the number");
        let (accepted, _) = l.accept().unwrap();
        assert_eq!(accepted.local_addr().unwrap().port(), s.peer_addr().unwrap().port());
        // And unstated outside prod really is the kernel's choice.
        let eph = listen("127.0.0.1", None, "r8").unwrap();
        assert_ne!(eph.local_addr().unwrap().port(), 0);
        assert!(matches!(listen("127.0.0.1", None, FACES_ENV), Err(SeamError::PortRequired { .. })));
    }

    // ── the registration, end to end ───────────────────────────────────────

    static COUNTS: Counter = Counter::new();

    struct Fake {
        se: ServiceEnv,
        addr: SocketAddr,
    }

    impl ServesWarnings for Fake {
        const SHAPE: &'static str = env::warnings::SHAPE;
        fn warnings(&self, asked: &str) -> anyhow::Result<Export> {
            if !asked.is_empty() && asked != Self::SHAPE {
                anyhow::bail!("this service serves `{}`, not `{asked}`", Self::SHAPE);
            }
            Ok(COUNTS.export(&self.se.service, Self::SHAPE, "1700000000"))
        }
    }

    impl Service for Fake {
        const VERBS: &'static [Verb<Fake>] = &[Verb::new("PING", |_, c, _| Ok(c.line("PONG")?))];
        fn service_env(&self) -> &ServiceEnv {
            &self.se
        }
        fn addr(&self) -> SocketAddr {
            self.addr
        }
    }

    /// A second service serving a DIFFERENT shape, so a mismatched attestation
    /// can be minted at all.
    #[allow(dead_code)] // never constructed: only its attestation is minted
    struct Other(ServiceEnv);
    impl ServesWarnings for Other {
        const SHAPE: &'static str = "other-warnings";
        fn warnings(&self, _: &str) -> anyhow::Result<Export> {
            anyhow::bail!("unused")
        }
    }
    impl Service for Other {
        const VERBS: &'static [Verb<Other>] = &[];
        fn service_env(&self) -> &ServiceEnv {
            &self.0
        }
        fn addr(&self) -> SocketAddr {
            "127.0.0.1:1".parse().unwrap()
        }
    }

    fn service_env(root: &std::path::Path, token: &str) -> ServiceEnv {
        let data_root = root.join("data/t");
        std::fs::create_dir_all(data_root.join("fake")).unwrap();
        let data_root = data_root.canonicalize().unwrap();
        ServiceEnv {
            service: "fake".into(),
            env: "t".into(),
            data_dir: data_root.join("fake"),
            seams: data_root.join("seams.json"),
            data_root,
            boot_token: token.into(),
            version: "test-label".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            dirty: false,
            bind: "127.0.0.1:0".into(),
        }
    }

    fn say(addr: &SocketAddr, line: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        writeln!(s, "{line}").unwrap();
        let mut out = String::new();
        BufReader::new(s).read_line(&mut out).unwrap();
        out.trim().to_string()
    }

    /// The row is written by the service, after its bind, carrying this start's
    /// token — and cauldron's OWN probe (`env::seams::probe`, the HELLO
    /// handshake) and OWN compliance check (`env::warnings::check`) call it
    /// live and compliant. Those two are the supervisor's instruments, not this
    /// test's, so the test cannot agree with itself.
    #[test]
    fn a_producer_registers_through_the_seam_and_cauldron_calls_it_live() {
        let root = tempfile::tempdir().unwrap();
        let se = service_env(root.path(), "boot-token-r8");
        let attested = env::warnings::attest_warnings!(Fake);
        let reg = Registration { binary: "/x/fake".into(), probe: "seam".into() };
        let se2 = se.clone();
        let (_svc, addr) = start(&se, &attested, &reg, move |addr| Fake { se: se2, addr }).unwrap();

        let table = Table::load(&se.env, &se.data_root, &se.seams).unwrap();
        let row = table.seams.get("fake").expect("the service wrote its row");
        assert!(row.registered, "a registered row, not a hosted one");
        assert_eq!((row.addr.clone(), row.boot_token.as_str()), (addr.to_string(), "boot-token-r8"));
        assert_eq!(row.binary, "/x/fake");

        let verdict = env::seams::probe(row);
        assert_eq!(verdict.state, SeamState::Live, "{}", verdict.detail);

        COUNTS.record(Level::Warn, "src/fake.rs", 3, "counted");
        let c = env::warnings::check(row, &verdict, Some(env::warnings::SHAPE));
        assert!(!c.is_violation(), "{:?}", c.missing());
        let export = c.export().expect("served");
        assert_eq!((export.producer.as_str(), export.counts_since.as_str()), ("fake", "1700000000"));
        assert!(export.warn >= 1);

        assert!(say(&addr, "WARNINGS gossip").starts_with("ERR "), "another shape was answered");
        assert_eq!(say(&addr, "PING"), "PONG", "the service's own verb");
        assert!(say(&addr, "DATA").starts_with("ERR unknown request"), "a word nobody owns");
    }

    /// The HELLO handshake is the evidence, so a row whose token is not the
    /// running instance's must be called stale by the same probe — the
    /// counterpart that shows `Live` above could have come out differently.
    #[test]
    fn a_row_with_another_token_is_not_live() {
        let root = tempfile::tempdir().unwrap();
        let se = service_env(root.path(), "the-real-token");
        let attested = env::warnings::attest_warnings!(Fake);
        let reg = Registration { binary: String::new(), probe: "seam".into() };
        let se2 = se.clone();
        let (_svc, _addr) = start(&se, &attested, &reg, move |addr| Fake { se: se2, addr }).unwrap();
        let table = Table::load(&se.env, &se.data_root, &se.seams).unwrap();
        let mut row = table.seams.get("fake").unwrap().clone();
        row.boot_token = "a-token-nobody-holds".into();
        let verdict = env::seams::probe(&row);
        assert_ne!(verdict.state, SeamState::Live, "{}", verdict.detail);
    }

    /// An attestation minted for another shape is refused BEFORE anything is
    /// bound or written; the matching one is accepted by the same call (above).
    #[test]
    fn an_attestation_for_another_shape_is_refused_and_nothing_is_written() {
        let root = tempfile::tempdir().unwrap();
        let se = service_env(root.path(), "t");
        let wrong = env::warnings::attest_warnings!(Other);
        let reg = Registration { binary: String::new(), probe: "seam".into() };
        let se2 = se.clone();
        let Err(e) = start(&se, &wrong, &reg, move |addr| Fake { se: se2, addr }) else {
            panic!("a start with another shape's attestation was accepted")
        };
        assert_eq!(e, SeamError::WrongAttestation { attested: "other-warnings", serves: "warnings" });
        assert!(!se.seams.exists(), "no row was written for a refused start");
    }
}
