//! **THE WEBSOCKET PORT, THROUGH THE REAL PATH**: `store/config.toml` ->
//! [`tts::Config::ws_addr`] -> the bind in [`tts::daemon::run`] -> the seam's
//! `WS` verb. Every test here drives `run` itself, as `cauldron start` does.
//!
//! # Why this file exists, and what shape it had to have
//!
//! Nothing tested this wiring, and replacing the bind with `127.0.0.1:0`
//! regardless of the config left all 146 tests green. In `prod` that is the
//! whole of the operator's voice: the faces dial a fixed port and read no
//! seam, so a daemon on a port of its own choosing is a healthy process he
//! cannot hear.
//!
//! The reason a green suite missed it is the reason this file is written the
//! way it is. **A test that asserts "a listener exists" passes on `:0`.** So
//! the assertion is an EQUALITY between the port the config STATED and the
//! port the kernel actually BOUND — and the two operands reach it by
//! independent routes:
//!
//! * the wanted port is picked BELOW the kernel's ephemeral range (see
//!   [`pick_low_port`]) and written into `store/config.toml` as text, so a
//!   daemon that ignored the config could not be handed it by accident;
//! * the bound port is read back off the running daemon's own seam (`WS`),
//!   which the daemon derived from `TcpListener::local_addr` after binding.
//!
//! Neither is computed from the other. And because the picked port changes
//! every run, [`the_bound_port_is_the_configured_port`] configures TWO
//! DIFFERENT ports in one test: an implementation that bound anything of its
//! own choosing would have to coincide twice. That is the measurement being
//! able to come out differently, which is what the arc's two false greens
//! both lacked — an assertion that read one stamp for both operands and so
//! proved `==` reflexive, and a dirt check forced red from inside its own
//! scope, which confirmed itself.
//!
//! **No test here binds 3652, 3651 or 3653.** The operator's `tts` holds 3652
//! right now; binding it would fail as address-in-use even if it were
//! allowed, which it is not. The positive control is two distinct free ports,
//! not the production one.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

use env::seams::{ServiceEnv, Table};
use serde_json::Value;

/// A service environment under a temp data root, as `cauldron start` builds
/// one. `bind` is the SEAM's address and is always ephemeral; the websocket's
/// is the thing under test and comes from the config.
fn service_env(root: &Path, env: &str) -> ServiceEnv {
    let data_root = root.join("data").join(env);
    std::fs::create_dir_all(data_root.join("tts/store")).unwrap();
    let data_root = data_root.canonicalize().unwrap();
    ServiceEnv {
        service: "tts".into(),
        env: env.into(),
        data_dir: data_root.join("tts"),
        seams: data_root.join("seams.json"),
        data_root,
        boot_token: "run-path-token".into(),
        version: "test".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        dirty: false,
        bind: "127.0.0.1:0".into(),
    }
}

/// An ephemeral port, for the sentinel below: the kernel picks it.
fn pick_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// **A free port BELOW the ephemeral range, which is what makes the equality
/// unfakeable.** Operand one, chosen outside the daemon.
///
/// Measured on this machine: `net.inet.ip.portrange.first = 49152`, so a
/// `127.0.0.1:0` bind is handed something at or above that and can never be
/// handed one of these. A port picked ephemerally would not do: the first
/// version of this test configured one, released it, and the forcing that
/// bound `:0` regardless was handed the ADJACENT port — 50869 against a
/// configured 50868. It went red by one, and had the kernel reused the number
/// it would have gone GREEN against a daemon ignoring its config entirely.
/// A low port cannot coincide, so the red is structural rather than lucky.
///
/// Never 3651, 3652 or 3653: those are the operator's, and `tts` holds 3652.
fn pick_low_port() -> u16 {
    const EPHEMERAL_FIRST: u16 = 49_152;
    let base = 20_000 + (std::process::id() % 10_000) as u16;
    for port in base..base + 400 {
        assert!(port < EPHEMERAL_FIRST, "a configured port must sit below the ephemeral range");
        if [3651, 3652, 3653].contains(&port) {
            continue;
        }
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            drop(l);
            return port;
        }
    }
    panic!("no free port below {EPHEMERAL_FIRST} in {base}..{}", base + 400);
}

fn state_port(se: &ServiceEnv, port: u16) {
    std::fs::write(se.data_dir.join("store/config.toml"), format!("port = {port}\n")).unwrap();
}

/// Start `run`, wait until it has registered its seam, and ask that seam `WS`.
///
/// Operand two: the daemon's own report of the address it bound, which
/// `server::serve` read from `TcpListener::local_addr` — never from the
/// config.
async fn bound_ws(se: ServiceEnv) -> (SocketAddr, tokio::task::JoinHandle<anyhow::Result<()>>) {
    let (name, root, seams) = (se.env.clone(), se.data_root.clone(), se.seams.clone());
    let task = tokio::spawn(tts::daemon::run(se));
    let seam_addr = loop {
        if let Some(row) = Table::load(&name, &root, &seams).ok().and_then(|t| t.seams.get("tts").cloned()) {
            break row.addr;
        }
        assert!(!task.is_finished(), "run exited before it registered a seam");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let mut s = TcpStream::connect(&seam_addr).unwrap();
    writeln!(s, "WS").unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    let reported = line.trim().trim_start_matches("ws://").trim_end_matches("/ws");
    (reported.parse().unwrap_or_else(|e| panic!("the seam reported {line:?}: {e}")), task)
}

/// `GET /health` on a port, so "the right port" is not satisfied by a stray
/// listener that happens to hold it.
///
/// **Bounded, and it reads to the body rather than to EOF.** A blocking read
/// with no deadline turns a server that holds the connection open into a hung
/// test rather than a failed one, and a hung test reports nothing at all. The
/// request says `Connection: close` so the server is asked to hang up; the
/// timeout is what makes this honest if it does not.
fn health(port: u16) -> Value {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap_or_else(|e| panic!("nothing is listening on {port}: {e}"));
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut w = s.try_clone().unwrap();
    w.write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    let mut r = s;
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                // The body is one small JSON object; stop at its close rather
                // than waiting for a hang-up that may not come.
                if out.iter().rev().take_while(|b| b.is_ascii_whitespace() || **b == b'}').any(|b| *b == b'}') {
                    break;
                }
            }
            Err(e) => panic!("reading /health on {port}: {e}"),
        }
    }
    let text = String::from_utf8_lossy(&out).to_string();
    serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap_or_default())
        .unwrap_or_else(|e| panic!("/health on {port} answered {text:?}: {e}"))
}

/// **THE PREDICATE: the port the daemon bound IS the port the config stated.**
///
/// `flavor = "multi_thread"`, like every test below, because this one drives
/// the REAL `run`: `main.rs` builds a multi-thread runtime, and on
/// `#[tokio::test]`'s default current-thread one the blocking read of
/// `/health` below starves the axum task that has to answer it — a hung test
/// rather than a failing one.
///
/// Twice, with two different ports, so tracking is required rather than
/// coincidence. Fails when the bind ignores the config — including the `:0`
/// that 146 green tests allowed.
#[tokio::test(flavor = "multi_thread")]
async fn the_bound_port_is_the_configured_port() {
    common::set_env();
    let mut configured = Vec::new();
    for _ in 0..2 {
        let root = tempfile::tempdir().unwrap();
        let se = service_env(root.path(), "r3s3");
        let want = pick_low_port();
        state_port(&se, want);
        let (got, task) = tokio::time::timeout(Duration::from_secs(20), bound_ws(se)).await.expect("run came up");
        assert_eq!(
            got.port(),
            want,
            "config.toml stated port {want} and the daemon bound {got}: the faces dial the \
             stated port and read no seam, so this is a daemon nobody can hear"
        );
        // …and the thing holding that port is THIS daemon, not a stray.
        assert_eq!(health(want)["daemon"], "tts");
        eprintln!("configured {want} -> bound {got}");
        configured.push(want);
        task.abort();
    }
    assert_ne!(
        configured[0], configured[1],
        "both runs configured the same port, so one coincidence would have satisfied two \
         assertions — the input has to vary for the equality to mean anything"
    );
}

/// The other half of the contract: with no port stated, a non-`prod`
/// environment gets the kernel's choice. Asserted against a port picked the
/// same way the test above picks one, so "ephemeral" is a real difference
/// rather than the absence of a check.
#[tokio::test(flavor = "multi_thread")]
async fn an_unstated_port_outside_prod_is_ephemeral_and_reachable() {
    common::set_env();
    let root = tempfile::tempdir().unwrap();
    // HELD OPEN for the whole run, not picked and released: a released port
    // can be handed straight back by the kernel, which would make this
    // inequality flake rather than mean anything.
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let not_this = held.local_addr().unwrap().port();
    let (got, task) = tokio::time::timeout(Duration::from_secs(20), bound_ws(service_env(root.path(), "r3s3")))
        .await
        .expect("run came up");
    assert_ne!(got.port(), not_this, "the daemon bound a port something else is holding");
    drop(held);
    assert_eq!(health(got.port())["daemon"], "tts");
    task.abort();
}

/// OPPOSED: in an environment named `prod` an unstated port is REFUSED by
/// `run`, before anything is bound and before any seam is written — because
/// there the ephemeral answer is the one that costs him his voice.
#[tokio::test(flavor = "multi_thread")]
async fn an_unstated_port_in_prod_is_refused_before_anything_binds() {
    let root = tempfile::tempdir().unwrap();
    let se = service_env(root.path(), "prod");
    let seams = se.seams.clone();
    let e = tokio::time::timeout(Duration::from_secs(10), tts::daemon::run(se))
        .await
        .expect("run returned")
        .expect_err("prod started on a port of the daemon's choosing");
    assert!(e.to_string().contains("port = 3652"), "the refusal must name the fix: {e:#}");
    assert!(!seams.exists(), "a refused run registered a seam");
}
