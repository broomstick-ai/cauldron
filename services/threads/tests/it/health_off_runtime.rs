//! **`/health` ANSWERED OFF THE RUNTIME** (R9.C7i, implements C-7; DEPLOY-9j F1).
//!
//! Against the REAL binary on a throwaway data dir and kernel ports. The prod
//! stall (every tokio worker parked in a synchronous call) is made with the
//! `test-seams` fault `THREADS_RECOVERY_FAULT=block`, which puts one
//! `std::thread::sleep` on each worker — `hang` is `pending()` and yields, so
//! it cannot starve anything.
//!
//! * G1 a starved runtime still answers on the door, fast, with its lag — and
//!   the chassis `/health` in the same moment is the prod read timeout;
//! * G2 a free runtime's lag is small (opposes a constant-large lag);
//! * G3 503 `stalled` only past the declared bound (4 s stall vs 2 s bound),
//!   and 200 under it (1 s stall vs the same bound);
//! * G4 a killed daemon is REFUSED on the door, not slow;
//! * G5 the shipped `services/threads.toml` points `[launch] health` at the
//!   door and `[quiet]` at the chassis, and every other http-probed service is
//!   named with whether its `/health` shares a runtime;
//! * G6 SIGTERM under starvation is logged as unheard by the door; opposed by
//!   a free runtime mid-drain, which hears it and logs nothing of the kind.
//!
//! Nothing here touches `:3653`, `~/.cauldron` or a real claude.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

static SERIAL: Mutex<()> = Mutex::new(());

/// cauldron's own probe timeout (`libs/env/src/seams.rs` `PROBE_TIMEOUT`).
const PROBE: Duration = Duration::from_millis(1500);

fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// How a `GET /health` went: the answer, or which half failed.
#[derive(Debug)]
enum Got {
    Answer(u16, Value),
    /// The connect itself failed (nothing listening: refused, or no route).
    Connect(std::io::ErrorKind),
    /// Connected and wrote, then no status line (the prod `os error 35`).
    Read(std::io::ErrorKind),
}

fn get_health(hostport: &str, timeout: Duration) -> Got {
    let sa: SocketAddr = hostport.to_socket_addrs().unwrap().next().unwrap();
    let mut s = match TcpStream::connect_timeout(&sa, timeout) {
        Ok(s) => s,
        Err(e) => return Got::Connect(e.kind()),
    };
    s.set_read_timeout(Some(timeout)).unwrap();
    // The request cauldron's holder sends (`hosting::send_request`): one write.
    let req = format!("GET /health HTTP/1.0\r\nHost: {hostport}\r\nConnection: close\r\n\r\n");
    if let Err(e) = s.write_all(req.as_bytes()) {
        return Got::Read(e.kind());
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if buf.is_empty() => return Got::Read(e.kind()),
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let Some(code) = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()) else {
        return Got::Read(std::io::ErrorKind::InvalidData);
    };
    let body = text.split("\r\n\r\n").nth(1).and_then(|b| serde_json::from_str(b).ok()).unwrap_or(Value::Null);
    Got::Answer(code, body)
}

struct Daemon {
    child: Child,
    log: PathBuf,
    chassis: String,
    door: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("/bin/kill").args(["-KILL", &self.child.id().to_string()]).status();
        let _ = self.child.wait();
    }
}

impl Daemon {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn door(&self, timeout: Duration) -> Got {
        get_health(&self.door, timeout)
    }

    fn signal(&self, sig: &str) {
        let st = Command::new("/bin/kill").args([sig, &self.child.id().to_string()]).status().unwrap();
        assert!(st.success(), "kill {sig}");
    }

    /// Poll the door until `pred` holds on an answer; the answer.
    fn door_until(&self, within: Duration, what: &str, pred: impl Fn(u16, &Value) -> bool) -> (u16, Value) {
        let t0 = Instant::now();
        let mut last = None;
        loop {
            if let Got::Answer(c, b) = self.door(PROBE) {
                if pred(c, &b) {
                    return (c, b);
                }
                last = Some((c, b));
            }
            assert!(t0.elapsed() < within, "the door never answered {what} within {within:?} (last {last:?}); log:\n{}", self.log());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Boot the real binary: chassis on a free 127.0.0.1 port, the door on
/// `door` (or a free 127.0.0.1 port), plus `extra` env.
fn boot(scratch: &Path, door: Option<String>, chassis_port: Option<u16>, extra: &[(&str, &str)]) -> Daemon {
    let data_dir = scratch.join("data");
    std::fs::create_dir_all(data_dir.join("aa-owned")).unwrap();
    std::fs::create_dir_all(scratch.join("fake-state")).unwrap();
    let port = chassis_port.unwrap_or_else(free_port);
    let door = door.unwrap_or_else(|| format!("127.0.0.1:{}", free_port()));
    let cfg = scratch.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "host = \"127.0.0.1\"\nchassis_port = {port}\ndata_dir = \"{dd}\"\nclaude_bin = \"{fc}\"\n\
             calibrate = false\nedge_poll = false\nprobe_interval_secs = 0\nprobe_interval_busy_secs = 0\n\
             [accounts]\naa-owned = \"{dd}/aa-owned\"\n",
            dd = data_dir.display(),
            fc = fake_claude().display()
        ),
    )
    .unwrap();
    let log = scratch.join(format!("daemon-{port}.log"));
    let child = Command::new(env!("CARGO_BIN_EXE_threads"))
        .arg("--config")
        .arg(&cfg)
        .env_remove("THREADS_CHASSIS_PORT")
        .env_remove("THREADS_RUNTIME_STALL_SECS")
        .env_remove("THREADS_RECOVERY_FAULT")
        .env("THREADS_HEALTH_ADDR", &door)
        .env("THREADS_UPDATE_WATCH", "off")
        .env("THREADS_RESUME_TICK_MS", "60000")
        .env("CAULDRON_HOME", scratch.join("cauldron-home"))
        .env("THREADS_CLAUDE_BIN", fake_claude())
        .env("THREADS_CLAUDE_HOME", scratch.join("claude-home"))
        .env("HOME", scratch.join("home"))
        .env("FAKE_STATE_DIR", scratch.join("fake-state"))
        .env("RUST_LOG", "info")
        .envs(extra.iter().copied())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .expect("spawn the threads binary");
    Daemon { child, log, chassis: format!("127.0.0.1:{port}"), door }
}

fn lag(b: &Value) -> u64 {
    b["runtime_lag_ms"].as_u64().unwrap_or_else(|| panic!("no runtime_lag_ms in {b}"))
}

/// Every worker blocked: two workers, one blocker each.
fn starved(block_ms: &str) -> Vec<(&'static str, String)> {
    vec![
        ("THREADS_RECOVERY_FAULT", "block".into()),
        ("THREADS_RECOVERY_FAULT_BLOCK_MS", block_ms.into()),
        ("TOKIO_WORKER_THREADS", "2".into()),
    ]
}

fn as_refs<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    v.iter().map(|(k, x)| (*k, x.as_str())).collect()
}

// ── G1 ────────────────────────────────────────────────────────────────────

/// **G1: A STARVED-BUT-ALIVE RUNTIME ANSWERS ON THE DOOR.** Every worker is
/// in `std::thread::sleep` for 6 s. Asserted: the door answers 200 within
/// 200 ms with `runtime_lag_ms` ≥ 1000. SENTINEL, in the same stall: the
/// chassis `/health` CONNECTS and then gets no status line within cauldron's
/// 1.5 s — the prod `reading the answer … os error 35` — so the stall is real
/// and the two answers come from different things.
#[test]
fn g1_a_starved_runtime_answers_on_the_door_within_200ms_with_its_lag() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = tempfile::tempdir().unwrap();
    let env = starved("6000");
    let d = boot(s.path(), None, None, &as_refs(&env));
    d.door_until(Duration::from_secs(30), "a lag of 1 s", |c, b| c == 200 && lag(b) >= 1000);

    let t = Instant::now();
    let got = d.door(PROBE);
    let took = t.elapsed();
    let Got::Answer(code, body) = got else { panic!("the door did not answer a starved runtime: {got:?}\n{}", d.log()) };
    assert_eq!(code, 200, "{body}");
    assert!(lag(&body) >= 1000, "lag must show the stall: {body}");
    assert!(took < Duration::from_millis(200), "the door took {took:?}: {body}");
    assert_eq!(body["served_by"], "health-door", "{body}");

    // Sentinel: the chassis, asked the holder's way in the same stall.
    let chassis = get_health(&d.chassis, PROBE);
    println!("G1: door {code} lag {} ms in {took:?}; chassis in the same stall: {chassis:?}", lag(&body));
    assert!(
        matches!(chassis, Got::Read(std::io::ErrorKind::WouldBlock) | Got::Read(std::io::ErrorKind::TimedOut)),
        "the fixture did not starve the chassis (expected the prod read timeout): {chassis:?}\n{}",
        d.log()
    );
}

// ── G2 ────────────────────────────────────────────────────────────────────

/// **G2: THE LAG IS REAL, NOT CONSTANT.** Same binary, no fault: 3 s of
/// samples (outlasting the 250 ms stamp tick twelve times), every one 200
/// with `runtime_lag_ms` < 500. A constant-large lag is red here; G1 is red
/// for a constant-zero one.
#[test]
fn g2_a_free_runtime_reads_a_lag_under_500ms_across_3s_outlasting_tick_250ms() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = tempfile::tempdir().unwrap();
    let d = boot(s.path(), None, None, &[]);
    d.door_until(Duration::from_secs(30), "status ok", |c, b| c == 200 && b["status"] == "ok");
    let t0 = Instant::now();
    let mut n = 0;
    let mut max = 0;
    while t0.elapsed() < Duration::from_secs(3) {
        let got = d.door(PROBE);
        let Got::Answer(code, body) = got else { panic!("{got:?}") };
        assert_eq!(code, 200, "{body}");
        max = max.max(lag(&body));
        assert!(lag(&body) < 500, "a free runtime read a lag of {} ms: {body}", lag(&body));
        n += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("G2: {n} samples, max runtime_lag_ms {max}");
    assert!(n >= 20, "too few samples: {n}");
}

// ── G3 ────────────────────────────────────────────────────────────────────

/// The shrunk stall bound G3 runs under (`THREADS_RUNTIME_STALL_SECS`).
const STALL_BOUND_SECS: &str = "2";

/// **G3a: A 4 s STALL OUTLASTS THE 2 s BOUND (AND THE 250 ms TICK) ⇒ 503
/// `stalled`.** With the lag and the status it had before. And it is a
/// reading, not a latch: once the stall ends the door is 200 again.
#[test]
fn g3_stall_4s_outlasts_bound_2s_and_tick_250ms_answers_503_stalled() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = tempfile::tempdir().unwrap();
    let mut env = starved("4000");
    env.push(("THREADS_RUNTIME_STALL_SECS", STALL_BOUND_SECS.into()));
    let d = boot(s.path(), None, None, &as_refs(&env));
    let (code, body) = d.door_until(Duration::from_secs(30), "503", |c, _| c == 503);
    println!("G3a: {code} {body}");
    assert_eq!(body["status"], "stalled", "{body}");
    assert!(lag(&body) >= 2000, "503 before the bound: {body}");
    assert_eq!(body["stall_after_ms"], 2000, "{body}");
    assert!(body["last_status"].is_string(), "the stalled body names what it said before: {body}");
    d.door_until(Duration::from_secs(15), "200 after the stall", |c, b| c == 200 && lag(b) < 500);
}

/// **G3b: A 1 s STALL UNDER THE SAME 2 s BOUND STAYS 200, OUTLASTING THE
/// 250 ms TICK.** Every answer from boot to 1.5 s after the stall is 200.
/// SENTINEL: the largest lag seen is ≥ 700 ms, so the stall really happened
/// while this was looking (a fixture that never stalled is red, not green).
#[test]
fn g3_stall_1s_under_bound_2s_stays_200_outlasting_tick_250ms() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = tempfile::tempdir().unwrap();
    let mut env = starved("1000");
    env.push(("THREADS_RUNTIME_STALL_SECS", STALL_BOUND_SECS.into()));
    let d = boot(s.path(), None, None, &as_refs(&env));
    let t0 = Instant::now();
    let mut max = 0;
    let mut seen_at: Option<Instant> = None;
    loop {
        if let Got::Answer(code, body) = d.door(PROBE) {
            assert_eq!(code, 200, "a stall under the bound answered non-200: {body}\n{}", d.log());
            max = max.max(lag(&body));
            if max >= 700 && seen_at.is_none() {
                seen_at = Some(Instant::now());
            }
        }
        if seen_at.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "the fixture never stalled (max lag {max} ms)\n{}", d.log());
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("G3b: max runtime_lag_ms {max}, all 200");
}

// ── G4 ────────────────────────────────────────────────────────────────────

/// **G4: DEAD IS REFUSED, NOT SLOW.** Killed MID-STALL (so the runtime is not
/// what closes the door): before the kill the door answers 200; after it the
/// same connect is REFUSED at once — never a read timeout, so the door does
/// not outlive its process or answer for a corpse.
#[test]
fn g4_a_killed_daemon_is_refused_on_the_door_not_slow() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = tempfile::tempdir().unwrap();
    let env = starved("8000");
    let mut d = boot(s.path(), None, None, &as_refs(&env));
    d.door_until(Duration::from_secs(30), "a lag of 1 s", |c, b| c == 200 && lag(b) >= 1000);
    d.signal("-KILL");
    d.child.wait().unwrap();
    let t = Instant::now();
    let got = d.door(PROBE);
    let took = t.elapsed();
    println!("G4: after SIGKILL the door gave {got:?} in {took:?}");
    assert!(matches!(got, Got::Connect(std::io::ErrorKind::ConnectionRefused)), "{got:?}");
    assert!(took < Duration::from_millis(200), "refused, not slow: {took:?}");
}

// ── G5 ────────────────────────────────────────────────────────────────────

fn shipped(name: &str) -> toml::Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(format!("{name}.toml"));
    toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// As cauldron expands a hosted value (`cli/src/main.rs` `expand_hosted`).
fn expand(v: &str, addr: &str) -> String {
    let port = addr.rsplit(':').next().unwrap();
    v.replace("${CAULDRON_HOSTED_ADDR}", addr).replace("${CAULDRON_HOSTED_PORT}", port)
}

/// Every `probe = "http"` service the repository ships, and whether its
/// `/health` is served on the same runtime as its work. A new http-probed
/// service is red here until someone looks at its `/health` and adds it.
const HTTP_PROBED: &[(&str, &str)] = &[
    ("threads", "no: the off-runtime health door (R9.C7i)"),
    ("ui-server", "yes: 9010 is AUI's tokio proto-v2 listener (not examined further at R9.C7i)"),
];

/// **G5: WHO IS IN THE REGIME (doctrine 10).** The SHIPPED declaration,
/// parsed and expanded as cauldron expands it for a real port: `[launch]
/// health` asks the address `THREADS_HEALTH_ADDR` binds, which is not the
/// chassis address; then the real binary booted with exactly those expanded
/// values answers the health URL from the DOOR (`served_by`) and the `[quiet]`
/// read on the CHASSIS address (no `served_by`, the restart gate at its
/// pointer). Counterpart: every other http-probed service is named.
#[test]
fn g5_shipped_threads_toml_asks_the_door_for_health_and_the_chassis_for_quiet() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let t = shipped("threads");
    assert_eq!(t["probe"].as_str(), Some("http"));
    let launch = &t["launch"];
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let health = expand(launch["health"].as_str().unwrap(), &addr);
    let door = expand(t["env"]["THREADS_HEALTH_ADDR"].as_str().expect("[env] THREADS_HEALTH_ADDR"), &addr);
    assert_eq!(health, format!("http://{door}/health"), "[launch] health must ask the door the daemon binds");
    let health_hostport = health.strip_prefix("http://").unwrap().split('/').next().unwrap().to_string();
    assert_ne!(health_hostport, addr, "the health URL must not be the chassis door");
    assert_eq!(expand(t["env"]["THREADS_CHASSIS_PORT"].as_str().unwrap(), &addr), port.to_string());
    let quiet = &t["quiet"];
    let (qpath, qptr) = (quiet["path"].as_str().unwrap(), quiet["pointer"].as_str().unwrap());

    // Booted with the shipped, expanded values.
    let s = tempfile::tempdir().unwrap();
    let d = boot(s.path(), Some(door.clone()), Some(port), &[]);
    let (c, b) = d.door_until(Duration::from_secs(30), "status ok", |c, b| c == 200 && b["status"] == "ok");
    assert_eq!(b["served_by"], "health-door", "{c} {b}");
    let got = get_health(&health_hostport, PROBE);
    assert!(matches!(&got, Got::Answer(200, b) if b["served_by"] == "health-door"), "health URL {health}: {got:?}");
    assert_eq!(qpath, "/health");
    let q = get_health(&addr, PROBE);
    let Got::Answer(200, qb) = q else { panic!("[quiet] on the chassis {addr}: {q:?}") };
    assert!(qb.get("served_by").is_none(), "the quiet read must be the chassis's own /health: {qb}");
    assert!(qb.pointer(qptr).is_some_and(Value::is_boolean), "{qptr} in {qb}");

    // Every http-probed declaration, named.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let name = p.file_stem().unwrap().to_str().unwrap().to_string();
        if shipped(&name).get("probe").and_then(|v| v.as_str()) == Some("http") {
            found.push(name);
        }
    }
    found.sort();
    let named: Vec<String> = HTTP_PROBED.iter().map(|(n, _)| n.to_string()).collect();
    println!("G5: http-probed {found:?}; shares runtime: {HTTP_PROBED:?}");
    assert!(found.len() >= 2, "the scan reached the shipped set: {found:?}");
    assert_eq!(found, named, "an http-probed service whose /health nobody has examined");
}

// ── G6 ────────────────────────────────────────────────────────────────────

const UNHEARD: &str = threads::health_door::UNHEARD_STOP_LOG;

/// **G6: SIGTERM UNDER STARVATION IS LOGGED, BY THE DOOR, AS UNHEARD.** All
/// workers blocked for 6 s; SIGTERM ~1 s in. The door's line lands BEFORE
/// the runtime's own `stop requested`; the time to exit is recorded (it is
/// expected not to drain until the stall ends).
#[test]
fn g6_sigterm_under_starvation_is_logged_unheard_by_the_door() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = tempfile::tempdir().unwrap();
    let env = starved("6000");
    let mut d = boot(s.path(), None, None, &as_refs(&env));
    d.door_until(Duration::from_secs(30), "a lag of 1 s", |c, b| c == 200 && lag(b) >= 1000);
    let t = Instant::now();
    d.signal("-TERM");
    let st = loop {
        if let Some(st) = d.child.try_wait().unwrap() {
            break st;
        }
        assert!(t.elapsed() < Duration::from_secs(90), "never exited; log:\n{}", d.log());
        std::thread::sleep(Duration::from_millis(20));
    };
    let took = t.elapsed();
    let log = d.log();
    println!("G6: SIGTERM -> exit {st:?} in {took:?}");
    let unheard = log.find(UNHEARD).unwrap_or_else(|| panic!("no unheard-stop line; log:\n{log}"));
    let heard = log.find("stop requested").unwrap_or_else(|| panic!("the runtime never heard it; log:\n{log}"));
    assert!(unheard < heard, "the door's line must come while the runtime is deaf; log:\n{log}");
}

/// **G6b, THE OPPOSITE ARM: A FREE RUNTIME HEARS SIGTERM AND NOTHING IS
/// LOGGED UNHEARD, OUTLASTING THE 1 s GRACE.** Recovery hangs (`hang` yields,
/// so the runtime is free) and holds a request permit, so the drain keeps the
/// process alive for its 45 s; 3 s after SIGTERM the log has `stop requested`
/// and no unheard line. A door that logs on every SIGTERM is red here.
#[test]
fn g6b_sigterm_on_a_free_runtime_mid_drain_is_heard_and_not_logged_unheard_outlasting_grace_1s() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = tempfile::tempdir().unwrap();
    let d = boot(s.path(), None, None, &[("THREADS_RECOVERY_FAULT", "hang")]);
    d.door_until(Duration::from_secs(30), "recovering", |c, b| c == 200 && b["status"] == "recovering");
    d.signal("-TERM");
    std::thread::sleep(Duration::from_secs(3));
    let log = d.log();
    assert!(log.contains("health door: SIGTERM received"), "the door saw no SIGTERM (the arm proves nothing); log:\n{log}");
    assert!(log.contains("stop requested"), "log:\n{log}");
    assert!(!log.contains(UNHEARD), "a heard stop was logged unheard; log:\n{log}");
    let got = d.door(PROBE);
    assert!(matches!(&got, Got::Answer(200, b) if b["stopping"] == true), "mid-drain door: {got:?}");
}

#[path = "support/first_exec.rs"]
mod first_exec;
