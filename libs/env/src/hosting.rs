//! **R5.S3 — what a declaration needs so cauldron can host EVERY service.**
//!
//! H179 ruling 1: *"Cauldron is up and hosts all services. It registers them,
//! and it supervises and restarts any that are down."* On the day this file
//! was written two services on the machine were still launched by launchd
//! rather than by cauldron, and each was blocked on something the declaration
//! form could not say:
//!
//! * **ui-server** (AUI) is a macOS BUNDLE binary whose identity is the
//!   bundle's signature, needs a working directory, and has a health check
//!   that a TCP connect cannot express — its 127.0.0.1:9009 is a singleton
//!   lock, and until harness `b4510544` a connect check against it filled the
//!   accept queue and read a healthy process as down.
//! * **soup-backup** is not a daemon at all: it is a short pass launchd runs
//!   every 300 s. Cauldron had no scheduler, so a `kind = "job"` could only be
//!   registered, never hosted.
//!
//! So this module adds, in one place so the siblings editing the supervisor
//! do not collide with it:
//!
//! 1. **`restart`** — a per-service restart policy (`always` | `never`), the
//!    inner switch under the environment's `auto_restart`.
//! 2. **`[schedule]`** — what makes a `job` something cauldron runs itself.
//! 3. **`probe = "http"` + `[launch] health`** — a health check that has to
//!    answer 200, so "listening" and "healthy" stop being the same word.
//! 4. **`[launch] cwd`** — the child's working directory.
//! 5. **a bundle artifact** — `[build] artifact` may name a `.app` directory,
//!    copied whole (signature seal intact) with `bin` naming the executable
//!    inside it.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::decl::{Build, Declaration};

/// The restart policy words. A closed set: a typo is refused at load time
/// rather than read as "not always".
pub struct Restart;
impl Restart {
    /// Relaunch it when it is down (subject to the environment's
    /// `auto_restart`). For a job: a run that FAILED is re-run after
    /// `retry_secs` instead of waiting for the next interval.
    pub const ALWAYS: &'static str = "always";
    /// Never relaunch it after the supervisor saw it die. It is still
    /// LAUNCHED once when the daemon finds it pinned and never started by this
    /// incarnation — "never" is about restarts, not about running at all. For
    /// a job: a failed run waits for the next scheduled tick.
    pub const NEVER: &'static str = "never";
}

pub fn default_restart() -> String {
    Restart::ALWAYS.into()
}

/// **`[schedule]` — when a `kind = "job"` runs.**
///
/// A job is a short pass that exits, not a process that stays up, so the
/// daemon's liveness loop does not apply to it. The daemon instead runs it
/// every `every_secs`, records every run and its exit in `supervisions`, and
/// keeps the last outcome in `data/<env>/jobs.json` for `status`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Schedule {
    /// Seconds between the START of one run and the start of the next. Set
    /// this OR `at`, never both. Due when EITHER the monotonic or the wall
    /// clock says so (R5.S4b), so a machine sleep does not delay it.
    #[serde(default)]
    pub every_secs: u64,
    /// **A DAILY CALENDAR TIME**, local, `"HH:MM"` — launchd's
    /// `StartCalendarInterval {Hour, Minute}` (the account-expiry alarm runs
    /// at `"09:30"`). Set this OR `every_secs`, never both. Read off the WALL
    /// clock every poll (R5.S4b): a slot slept through runs once on wake, a
    /// clock stepped back never re-runs one, and the last slot run survives a
    /// holder restart (`jobs.json` `last_slot`). See the hosts-all runbook §E.
    #[serde(default)]
    pub at: Option<String>,
    /// Arguments the image is run with for a pass (`["run"]` for soup-backup).
    #[serde(default)]
    pub args: Vec<String>,
    /// **THE JOB'S HEALTH CHECK.** Arguments the image is run with to ask
    /// whether it is healthy; exit 0 = healthy, anything else = not. Empty
    /// means the health is the last run's exit code alone.
    #[serde(default)]
    pub health_args: Vec<String>,
    /// Paths that must exist before a run starts (`${CAULDRON_SERVICE_DATA_DIR}`
    /// expands). A missing one BLOCKS the run and `status` says which — so a
    /// job whose config is env-relative never falls back to a default that
    /// names another environment's data.
    #[serde(default)]
    pub requires: Vec<String>,
    /// How long after a FAILED run (non-zero, or killed) it is re-run when
    /// `restart = "always"`. Default 30.
    #[serde(default = "default_retry")]
    pub retry_secs: u64,
    /// A run still going after this long is killed (and counts as failed).
    /// Default 600; never 0.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Run once as soon as the daemon finds it pinned (launchd's `RunAtLoad`).
    /// Default true. R5.S6: a holder RESTART is not a new load — an interval
    /// job whose last recorded pass started less than `every_secs` ago is not
    /// re-run at load (`supervisions` records a `skip`, why `recent`); one
    /// that never ran, whose last pass failed, or that is overdue runs at once.
    #[serde(default = "default_true")]
    pub run_at_load: bool,
}

fn default_retry() -> u64 {
    30
}
fn default_timeout() -> u64 {
    600
}
fn default_true() -> bool {
    true
}

/// Everything the R5.S3 keys can contradict, checked at load with the rest of
/// [`Declaration::validate`].
pub fn validate(d: &Declaration) -> Result<()> {
    match d.restart.as_str() {
        Restart::ALWAYS | Restart::NEVER => {}
        other => bail!(
            "REFUSED: `{}` declares `restart = \"{other}\"`. The policies are `always` (the \
             supervisor relaunches it when it is down) and `never` (it reports it down and \
             leaves it).",
            d.name
        ),
    }
    let job = d.kind == "job";
    match (&d.schedule, job) {
        (Some(_), false) => bail!(
            "REFUSED: `{}` has a `[schedule]` but `kind = \"{}\"`. A schedule is what a `job` \
             runs on; a daemon stays up and is watched instead.",
            d.name,
            d.kind
        ),
        (Some(s), true) => {
            match (s.every_secs, &s.at) {
                (0, None) => bail!(
                    "REFUSED: `{}`'s `[schedule]` says neither `every_secs` (an interval) nor \
                     `at` (a daily time), so it says nothing about when it runs \
                     (`every_secs = 0` is not an interval).",
                    d.name
                ),
                (n, Some(_)) if n > 0 => bail!(
                    "REFUSED: `{}`'s `[schedule]` says both `every_secs` and `at`. One clock.",
                    d.name
                ),
                (_, Some(at)) => {
                    parse_at(at).with_context(|| format!("`{}`'s `[schedule] at`", d.name))?;
                }
                _ => {}
            }
            if s.timeout_secs == 0 {
                bail!(
                    "REFUSED: `{}` declares `[schedule] timeout_secs = 0`. A run with no bound \
                     is a run that can hold the job forever.",
                    d.name
                );
            }
            if d.registers {
                bail!(
                    "REFUSED: `{}` is a scheduled `job` and does not say `registers = false`. A \
                     job is a pass that exits; it has no listener to register.",
                    d.name
                );
            }
            if d.launch.is_some() {
                bail!(
                    "REFUSED: `{}` is a scheduled `job` with a `[launch]` section. A job has no \
                     address to be reached at.",
                    d.name
                );
            }
            if d.probe != "none" {
                bail!(
                    "REFUSED: `{}` is a scheduled `job` with `probe = \"{}\"`. A job's health is \
                     its run's exit code and `[schedule] health_args`; say `probe = \"none\"`.",
                    d.name,
                    d.probe
                );
            }
        }
        _ => {}
    }
    let health = d.launch.as_ref().and_then(|l| l.health.as_ref());
    match (d.probe.as_str(), health) {
        ("http", None) => bail!(
            "REFUSED: `{}` declares `probe = \"http\"` and no `[launch] health` URL to ask.",
            d.name
        ),
        ("http", Some(u)) if !u.starts_with("http://") => bail!(
            "REFUSED: `{}`'s `[launch] health = \"{u}\"` is not an `http://host:port/path` URL.",
            d.name
        ),
        (p, Some(_)) if p != "http" => bail!(
            "REFUSED: `{}` declares a `[launch] health` URL with `probe = \"{p}\"`, which never \
             asks it. A health check nobody runs reads as coverage; say `probe = \"http\"`.",
            d.name
        ),
        _ => {}
    }
    if d.probe == "http" && d.registers {
        bail!(
            "REFUSED: `{}` declares `probe = \"http\"` and registers its own seam. The http \
             probe is for a HOSTED binary (`registers = false`); a registering service has the \
             stronger `seam` handshake.",
            d.name
        );
    }
    Ok(())
}

/// `"HH:MM"` -> seconds after local midnight.
pub fn parse_at(at: &str) -> Result<u64> {
    let (h, m) = at.split_once(':').with_context(|| format!("`{at}` is not HH:MM"))?;
    let (h, m): (u64, u64) = (
        h.parse().with_context(|| format!("`{at}` is not HH:MM"))?,
        m.parse().with_context(|| format!("`{at}` is not HH:MM"))?,
    );
    if h > 23 || m > 59 || at.len() != 5 {
        bail!("`{at}` is not a time of day as HH:MM");
    }
    Ok(h * 3600 + m * 60)
}

/// Seconds from `now_sod` (seconds after local midnight) until the next
/// `target_sod`; a target that is exactly now is a whole day away, so a run
/// that starts AT its time is not immediately due again.
pub fn secs_until(now_sod: u64, target_sod: u64) -> u64 {
    let d = (target_sod + 86_400 - now_sod % 86_400) % 86_400;
    if d == 0 {
        86_400
    } else {
        d
    }
}

/// `bin` is a file name, or — for a BUNDLE artifact only — a relative path
/// inside the version directory whose first component is the bundle's own
/// directory name (`AUI.app/Contents/MacOS/ui-server`).
pub fn validate_bin(service: &str, b: &Build) -> Result<()> {
    if !b.bin.contains('/') {
        return Ok(());
    }
    let p = Path::new(&b.bin);
    if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        bail!(
            "REFUSED: `{service}`'s `[build] bin = \"{}\"` must be a path INSIDE the version \
             directory: relative, and with no `..`.",
            b.bin
        );
    }
    let Some(artifact) = &b.artifact else {
        bail!(
            "REFUSED: `{service}`'s `[build] bin = \"{}\"` is a path, which only a BUNDLE \
             artifact may use (a cargo source installs one file by name).",
            b.bin
        );
    };
    let bundle = Path::new(artifact).file_name().map(|n| n.to_string_lossy().to_string());
    let first = p.components().next().map(|c| c.as_os_str().to_string_lossy().to_string());
    if bundle.is_none() || bundle != first {
        bail!(
            "REFUSED: `{service}`'s `[build] bin = \"{}\"` must start with the bundle's own \
             directory name `{}` — the artifact is installed whole under that name, and `bin` \
             names the executable inside it.",
            b.bin,
            bundle.unwrap_or_default()
        );
    }
    Ok(())
}

/// The part of a bundle `bin` path below the bundle directory:
/// `AUI.app/Contents/MacOS/ui-server` -> `Contents/MacOS/ui-server`.
pub fn bin_inside_bundle(bin: &str) -> &str {
    bin.split_once('/').map(|(_, rest)| rest).unwrap_or(bin)
}

/// The request `http_health` sends, in ONE `write` call (R5.S3b). `write!` on
/// an unbuffered `TcpStream` issues one `write` per format piece, so the
/// request left in several segments; a server that parses its first `read`
/// saw `GET ` alone (measured: 11 of 26 runs of the R5.S3 integration test
/// answered 503). A real server reads to the blank line, but one write is
/// what every client owes it.
fn send_request<W: Write>(w: &mut W, hostport: &str, path: &str) -> std::io::Result<()> {
    let req = format!("GET {path} HTTP/1.0\r\nHost: {hostport}\r\nConnection: close\r\n\r\n");
    w.write_all(req.as_bytes())
}

/// The `status` a service puts in its `/health` body to say it is ALIVE BUT
/// NOT READY (R9.C7h): threads answers `503 {"status": "migrating"}` while it
/// migrates its store behind a served port. Matched exactly, and only on a 503.
pub const ALIVE_NOT_READY: &str = "migrating";

/// **WHAT A HEALTH URL ANSWERED** (R9.C7h): the code, and — on a non-200 —
/// the `status` field of its JSON body, when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthAnswer {
    pub code: u16,
    pub status: Option<String>,
}

impl HealthAnswer {
    /// `503` with `status: "migrating"`: the service is alive and working
    /// toward ready, and must be neither killed nor relaunched for it. Its
    /// own bound (threads' stuck bound, 10 min) turns a migration that never
    /// finishes into `recovery_stuck`, which is NOT this.
    pub fn alive_not_ready(&self) -> bool {
        self.code == 503 && self.status.as_deref() == Some(ALIVE_NOT_READY)
    }

    /// The code the holder acts on: 200 for healthy AND for alive-not-ready,
    /// the real code otherwise. See [`http_health`].
    pub fn holder_code(&self) -> u16 {
        if self.alive_not_ready() {
            200
        } else {
            self.code
        }
    }
}

/// **ASK A HEALTH URL, AND READ WHAT IT SAID** (R9.C7h). As [`http_health`],
/// but returns the real code and, on a non-200, the body's `status`.
pub fn health_answer(url: &str, timeout: Duration) -> Result<HealthAnswer> {
    health_answer_over(url, timeout, |sa, timeout| {
        let s = TcpStream::connect_timeout(sa, timeout)?;
        s.set_read_timeout(Some(timeout))?;
        s.set_write_timeout(Some(timeout))?;
        Ok(s)
    })
}

/// **ASK A HEALTH URL.** `Ok(status)` for any HTTP answer; `Err` when nothing
/// answers in time or the answer is not HTTP. A 200 is the only healthy
/// answer — the caller decides, so a 404 from a wrong path reads as what it
/// is (unhealthy), never as "up because something answered".
///
/// HTTP/1.0, `Connection: close`, one request, bounded by `timeout` on
/// connect, write and read. No client crate: this is the whole of the HTTP
/// cauldron speaks.
///
/// **R9.C7h: ALIVE-NOT-READY READS AS 200 HERE.** A `503` whose body says
/// `status: "migrating"` ([`HealthAnswer::alive_not_ready`]) is returned as
/// `Ok(200)`, because every caller of this function is asking "may this child
/// be left running?" — the launch's `wait_healthy` kills a child that has not
/// answered 200 within `start_timeout_ms`, and a store migration may outlast
/// that. A caller that needs the real code asks [`health_answer`].
pub fn http_health(url: &str, timeout: Duration) -> Result<u16> {
    Ok(health_answer(url, timeout)?.holder_code())
}

/// `http_health`'s whole body over any connected stream (R5.S3c): the TCP
/// connect is the only part `http_health` supplies, so a test hands in a
/// stream that RECORDS each `write` call and asserts on what `http_health`
/// itself sent — not on a helper it might stop calling (R5.S3b inspector
/// finding 2: reverting the call site to `write!` left the old helper-only
/// guard green).
#[cfg(test)]
fn http_health_over<S: Read + Write>(
    url: &str,
    timeout: Duration,
    connect: impl FnOnce(&std::net::SocketAddr, Duration) -> std::io::Result<S>,
) -> Result<u16> {
    Ok(health_answer_over(url, timeout, connect)?.holder_code())
}

fn health_answer_over<S: Read + Write>(
    url: &str,
    timeout: Duration,
    connect: impl FnOnce(&std::net::SocketAddr, Duration) -> std::io::Result<S>,
) -> Result<HealthAnswer> {
    let rest = url
        .strip_prefix("http://")
        .with_context(|| format!("`{url}` is not an http:// URL"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let sa = hostport
        .to_socket_addrs()
        .with_context(|| format!("resolving `{hostport}`"))?
        .next()
        .with_context(|| format!("`{hostport}` resolved to nothing"))?;
    let mut s = connect(&sa, timeout).with_context(|| format!("nothing answered at {hostport}"))?;
    send_request(&mut s, hostport, path)?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    // Only the status line is needed; stop at the first line break.
    loop {
        let n = s.read(&mut chunk).with_context(|| format!("reading the answer from {url}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.contains(&b'\n') || buf.len() > 4096 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let line = text.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let code = match (parts.next(), parts.next().and_then(|c| c.parse::<u16>().ok())) {
        (Some(v), Some(code)) if v.starts_with("HTTP/") => code,
        _ => bail!("{url} answered something that is not an HTTP status line: {line:?}"),
    };
    if code == 200 {
        // A 200 is read exactly as before: the status line is the answer.
        return Ok(HealthAnswer { code, status: None });
    }
    // R9.C7h: anything else may say WHY in its body. `Connection: close`, so
    // the body ends at EOF; bounded at 16 KiB and by `timeout`, and a body that
    // cannot be read is no body — never an error, the code stands.
    while buf.len() < 16 * 1024 {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let status = text
        .split_once("\r\n\r\n")
        .and_then(|(_, body)| serde_json::from_str::<serde_json::Value>(body.trim()).ok())
        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string));
    Ok(HealthAnswer { code, status })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve_once(reply: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = l.accept() {
                let mut b = [0u8; 1024];
                let _ = c.read(&mut b);
                let _ = c.write_all(reply.as_bytes());
            }
        });
        addr
    }

    /// A connected stream that records every `write` call it is handed and
    /// answers with `reply`.
    struct Calls {
        writes: std::rc::Rc<std::cell::RefCell<Vec<Vec<u8>>>>,
        reply: std::io::Cursor<&'static [u8]>,
    }
    impl Write for Calls {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.writes.borrow_mut().push(b.to_vec());
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Read for Calls {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.reply.read(b)
        }
    }

    /// `http_health`'s body over a recording stream: (status, each write).
    fn ask_recorded(url: &str, reply: &'static [u8]) -> (u16, Vec<Vec<u8>>) {
        let writes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let w = writes.clone();
        let code = http_health_over(url, Duration::from_secs(2), move |_, _| {
            Ok(Calls { writes: w, reply: std::io::Cursor::new(reply) })
        })
        .unwrap();
        let got = writes.borrow().clone();
        (code, got)
    }

    /// R5.S3b, re-aimed in R5.S3c — deterministic half: the request
    /// `http_health` ITSELF sends goes out in ONE `write` call, and it is the
    /// whole request. Opposite arm: the status is read off the reply, not a
    /// constant (200 and 503 both come back).
    #[test]
    fn http_health_sends_its_request_in_one_write_call() {
        let (code, w) = ask_recorded("http://127.0.0.1:9/health", b"HTTP/1.0 200 OK\r\n\r\n");
        assert_eq!(code, 200);
        assert_eq!(w.len(), 1, "http_health split its request over {} writes: {:?}", w.len(), w);
        assert_eq!(w[0], b"GET /health HTTP/1.0\r\nHost: 127.0.0.1:9\r\nConnection: close\r\n\r\n".to_vec());
        let (code, w) = ask_recorded("http://127.0.0.1:9/other", b"HTTP/1.0 503 Sick\r\n\r\n");
        assert_eq!(code, 503);
        assert_eq!(w, vec![b"GET /other HTTP/1.0\r\nHost: 127.0.0.1:9\r\nConnection: close\r\n\r\n".to_vec()]);
    }

    /// R5.S3b — end to end: a server that makes ONE `read` call and answers
    /// 200 only if that read holds the whole request (path AND blank line),
    /// 503 otherwise. Its opposite arm: the same server asked for a different
    /// path answers 503, so it is not a constant 200.
    fn serve_once_strict(path: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = l.accept() {
                let mut b = [0u8; 1024];
                let n = c.read(&mut b).unwrap_or(0);
                let got = &b[..n];
                let whole = got.starts_with(format!("GET {path} ").as_bytes()) && got.ends_with(b"\r\n\r\n");
                let reply: &[u8] = if whole { b"HTTP/1.0 200 OK\r\n\r\n" } else { b"HTTP/1.0 503 Split\r\n\r\n" };
                let _ = c.write_all(reply);
            }
        });
        addr
    }

    #[test]
    fn a_server_that_reads_once_sees_the_whole_request() {
        for _ in 0..50 {
            let a = serve_once_strict("/health");
            assert_eq!(http_health(&format!("http://{a}/health"), Duration::from_secs(2)).unwrap(), 200);
            let a = serve_once_strict("/health");
            assert_eq!(http_health(&format!("http://{a}/other"), Duration::from_secs(2)).unwrap(), 503);
        }
    }

    #[test]
    fn a_200_is_read_as_200() {
        let a = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(http_health(&format!("http://{a}/health"), Duration::from_secs(2)).unwrap(), 200);
    }

    // ── R9.C7h: ALIVE-NOT-READY ─────────────────────────────────────────

    /// A 503 whose body says `migrating` is alive-not-ready: `http_health`
    /// (the launch's `wait_healthy`) reads it as 200, and `health_answer`
    /// keeps the real code. Opposed three ways, each a 503 that must stay
    /// 503: `recovery_stuck` (the bound fired), a 503 with no body (the old
    /// shape), and `migrating` under a different code.
    #[test]
    fn a_503_migrating_is_alive_not_ready_and_nothing_else_is() {
        let migrating = "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\n\r\n\
                         {\"status\":\"migrating\",\"migration\":{\"state\":\"migrating\"}}";
        let a = serve_once(migrating);
        assert_eq!(http_health(&format!("http://{a}/health"), Duration::from_secs(2)).unwrap(), 200);
        let a = serve_once(migrating);
        let ans = health_answer(&format!("http://{a}/health"), Duration::from_secs(2)).unwrap();
        assert_eq!(ans, HealthAnswer { code: 503, status: Some("migrating".into()) });
        assert!(ans.alive_not_ready());

        for reply in [
            "HTTP/1.1 503 Service Unavailable\r\n\r\n{\"status\":\"recovery_stuck\"}",
            "HTTP/1.0 503 Sick\r\n\r\n",
            "HTTP/1.1 500 Internal\r\n\r\n{\"status\":\"migrating\"}",
        ] {
            let a = serve_once(reply);
            let code = http_health(&format!("http://{a}/health"), Duration::from_secs(2)).unwrap();
            assert_ne!(code, 200, "{reply:?} must not read as up");
            let a = serve_once(reply);
            assert!(!health_answer(&format!("http://{a}/health"), Duration::from_secs(2)).unwrap().alive_not_ready());
        }
    }

    #[test]
    fn a_404_is_read_as_404_not_as_up() {
        let a = serve_once("HTTP/1.0 404 Not Found\r\n\r\n");
        assert_eq!(http_health(&format!("http://{a}/nope"), Duration::from_secs(2)).unwrap(), 404);
    }

    #[test]
    fn a_listener_that_says_nothing_http_is_an_error() {
        let a = serve_once("HELLO\n");
        assert!(http_health(&format!("http://{a}/"), Duration::from_secs(2)).is_err());
    }

    #[test]
    fn nothing_listening_is_an_error() {
        let a = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
        assert!(http_health(&format!("http://{a}/"), Duration::from_millis(300)).is_err());
    }

    #[test]
    fn a_daily_time_is_parsed_and_counted_down() {
        assert_eq!(parse_at("09:30").unwrap(), 34_200);
        assert!(parse_at("9:30").is_err() && parse_at("24:00").is_err() && parse_at("09:60").is_err());
        assert_eq!(secs_until(34_000, 34_200), 200);
        assert_eq!(secs_until(34_200, 34_200), 86_400);
        assert_eq!(secs_until(34_205, 34_200), 86_395);
    }

    #[test]
    fn bundle_bin_strips_the_bundle_dir() {
        assert_eq!(bin_inside_bundle("AUI.app/Contents/MacOS/ui-server"), "Contents/MacOS/ui-server");
    }
}
