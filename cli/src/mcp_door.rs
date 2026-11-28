//! **The command line's way to the `mcp` service**: find it, read its door
//! secret, and send it one JSON-RPC message. Used by the stdio bridge
//! (`cauldron mcp stdio`), by `cauldron soup import` when the service holds
//! the soup, and by `cauldron mcp status`.
//!
//! - The service is found through the environment's seam table: its `mcp`
//!   row must be LIVE (the same handshake `cauldron status` makes), and its
//!   seam's `MCP` verb answers the door's address (`http://127.0.0.1:<port>/mcp`).
//! - The secret is `<root>/data/<env>/mcp/door.secret`, minted by the
//!   service at every start (owner-only). It is read for each request and
//!   never printed, logged or put in an error message.
//! - Every dial goes through `env::egress::connect_tcp`, loopback only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use env::seams::{self, SeamState, Table};
use env::Environment;

/// The service's name: its seam row, its declaration, its data dir.
pub const SERVICE: &str = "mcp";

/// The door's one route (shapewire's MCP carrier).
pub const PATH: &str = "/mcp";

/// How long a request may take: a large `soup_put` judges every record.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// A running service: its pid, its seam, and its door.
#[derive(Debug, Clone)]
pub struct Found {
    pub pid: u32,
    pub seam: String,
    pub door: SocketAddr,
}

/// Why the service could not be reached.
#[derive(Debug, Clone)]
pub enum Absent {
    /// The seam table has no `mcp` row.
    NoRow,
    /// The row is there and its probe is not live (the state, and why).
    NotLive(String, String),
    /// The seam table cannot be read, or the seam did not say where the door is.
    Broken(String),
}

impl Absent {
    pub fn say(&self) -> String {
        match self {
            Absent::NoRow => "it is not running (no `mcp` row in the seam table)".to_string(),
            Absent::NotLive(state, why) => format!("it is not reachable ({state}: {why})"),
            Absent::Broken(why) => format!("it cannot be reached ({why})"),
        }
    }
}

/// The command that starts the service in `env`.
pub fn start_command(env: &str) -> String {
    format!("cauldron --env {env} start {SERVICE}")
}

/// Find the environment's running service.
pub fn find(e: &Environment) -> std::result::Result<Found, Absent> {
    let table = Table::load(&e.name, &e.data_root, &e.seams()).map_err(|err| Absent::Broken(format!("{err:#}")))?;
    let Some(row) = table.seams.get(SERVICE) else { return Err(Absent::NoRow) };
    let v = seams::probe(row);
    if v.state != SeamState::Live {
        return Err(Absent::NotLive(v.state.as_str().to_string(), v.detail));
    }
    let url = seam_ask(&row.addr, "MCP").map_err(|err| Absent::Broken(format!("asking its seam for the door: {err:#}")))?;
    let door = url
        .strip_prefix("http://")
        .and_then(|u| u.strip_suffix(PATH))
        .and_then(|a| a.parse::<SocketAddr>().ok())
        .filter(|a| a.ip().is_loopback())
        .ok_or_else(|| Absent::Broken(format!("its seam answered `{url}`, which is not a loopback door")))?;
    Ok(Found { pid: row.pid, seam: row.addr.clone(), door })
}

/// One line to the service's seam, one line back.
/// [`seam_ask`], waiting up to `wait` for the answer (an act queued behind
/// other writes). A wait that runs out is an error [`timed_out`] knows.
pub fn seam_ask_waiting(addr: &str, line: &str, wait: Duration) -> Result<String> {
    let sa: SocketAddr = addr.parse().with_context(|| format!("the seam address `{addr}`"))?;
    let s = env::egress::connect_tcp("mcp seam", &sa, Duration::from_secs(2))?;
    s.set_read_timeout(Some(wait))?;
    let mut w = s.try_clone()?;
    w.write_all(format!("{line}\n").as_bytes())?;
    w.flush()?;
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out)?;
    let out = out.trim().to_string();
    if out.is_empty() {
        bail!("{addr} said nothing to the request");
    }
    Ok(out)
}

/// Is `err` a read that ran out of time (the far side took the request and
/// has not answered)?
pub fn timed_out(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|e| matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)))
}

pub fn seam_ask(addr: &str, line: &str) -> Result<String> {
    seam_ask_within(addr, line, Duration::from_secs(5))
}

/// [`seam_ask`], waiting up to `wait` for the answer (a merge takes longer
/// than a decision).
pub fn seam_ask_within(addr: &str, line: &str, wait: Duration) -> Result<String> {
    let sa: SocketAddr = addr.parse().with_context(|| format!("the seam address `{addr}`"))?;
    let s = env::egress::connect_tcp("mcp seam", &sa, Duration::from_secs(2))?;
    s.set_read_timeout(Some(wait))?;
    let mut w = s.try_clone()?;
    w.write_all(format!("{line}\n").as_bytes())?;
    w.flush()?;
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out)?;
    let out = out.trim().to_string();
    if out.is_empty() {
        bail!("{addr} said nothing to `{line}`");
    }
    Ok(out)
}

/// Where the door secret is.
pub fn secret_path(e: &Environment) -> PathBuf {
    soup_door::grants::store_dir(e).join("door.secret")
}

/// The door secret, as the service minted it at its last start.
fn secret(e: &Environment) -> Result<String> {
    let p = secret_path(e);
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading the door secret {}", p.display()))?;
    let s = s.trim().to_string();
    if s.is_empty() {
        bail!("the door secret {} is empty", p.display());
    }
    Ok(s)
}

/// An HTTP answer: its status and its body.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub body: String,
}

/// The most the door reads in one request (`soup_door::catalogue`).
pub const BODY_LIMIT: usize = soup_door::catalogue::REQUEST_BODY_LIMIT;

/// `n` bytes as MiB, one decimal.
pub fn mib(n: usize) -> String {
    format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
}

/// **The connection to the door was lost after the request went out**: it
/// was closed, reset or cut before a whole answer came back (the service was
/// killed, or crashed, mid-call). Whatever the call was, the service may
/// have acted on it; the error chain of [`post`] and [`post_stream`] carries
/// this when that is what happened, and [`is_lost`] finds it.
#[derive(Debug)]
pub struct Lost(pub String);

impl std::fmt::Display for Lost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the connection to the mcp door was lost mid-call: {}", self.0)
    }
}

impl std::error::Error for Lost {}

/// Did [`post`] (or [`post_stream`]) lose its connection after sending?
pub fn is_lost(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.downcast_ref::<Lost>().is_some())
}

/// `r`, an error after the request started going out, marked [`Lost`].
fn lost<T>(r: Result<T>) -> Result<T> {
    r.map_err(|e| anyhow::Error::new(Lost(format!("{e:#}"))))
}

/// The JSON-RPC body of one `tools/call`.
pub fn call_body(tool: &str, arguments: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}}).to_string()
}

/// `POST /mcp` with the secret: one JSON-RPC message, one answer. A body
/// larger than the door reads ([`BODY_LIMIT`]) is refused here, before
/// anything is sent.
pub fn post(e: &Environment, door: SocketAddr, body: &str) -> Result<Answer> {
    if body.len() > BODY_LIMIT {
        bail!(
            "this request is {} bytes ({}), over the {} the mcp door reads in one request (`too_large`); nothing was \
             sent, so nothing was written. Send fewer records per call.",
            body.len(),
            mib(body.len()),
            mib(BODY_LIMIT)
        );
    }
    let secret = secret(e)?;
    let mut s = env::egress::connect_tcp("mcp door", &door, Duration::from_secs(5)).with_context(|| format!("connecting to the mcp door at {door}"))?;
    s.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    s.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json, text/event-stream\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n",
        path = PATH,
        port = door.port(),
        len = body.len()
    );
    // The secret goes on the wire and nowhere else. From the first byte on,
    // a failure is a connection lost mid-call (`Lost`): the service may have
    // acted on the request.
    lost((|| -> Result<Answer> {
        s.write_all(head.as_bytes())?;
        s.write_all(b"Authorization: Bearer ")?;
        s.write_all(secret.as_bytes())?;
        s.write_all(b"\r\n\r\n")?;
        s.write_all(body.as_bytes())?;
        s.flush()?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).with_context(|| format!("reading the mcp door's answer from {door}"))?;
        parse_http(&raw)
    })())
}

/// `POST <path>` with the secret: `head` (one JSON-RPC message, no
/// newline) on the first line, then `len` bytes of `content`, streamed, never
/// held whole. When the door answers before taking every byte (it refused
/// the call from its first line), its answer is still read.
pub fn post_stream(e: &Environment, door: SocketAddr, path: &str, head_line: &str, content: &mut dyn Read, len: u64) -> Result<Answer> {
    let secret = secret(e)?;
    let mut s = env::egress::connect_tcp("mcp door", &door, Duration::from_secs(5)).with_context(|| format!("connecting to the mcp door at {door}"))?;
    s.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    s.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let total = head_line.len() as u64 + 1 + len;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/octet-stream\r\n\
         Content-Length: {total}\r\nConnection: close\r\n",
        port = door.port(),
    );
    lost((|| -> Result<()> {
        s.write_all(head.as_bytes())?;
        s.write_all(b"Authorization: Bearer ")?;
        s.write_all(secret.as_bytes())?;
        s.write_all(b"\r\n\r\n")?;
        Ok(())
    })())?;
    let sent = (|| -> std::io::Result<u64> {
        s.write_all(head_line.as_bytes())?;
        s.write_all(b"\n")?;
        let n = std::io::copy(&mut content.take(len), &mut s)?;
        s.flush()?;
        Ok(n)
    })();
    let mut raw = Vec::new();
    let read = s.read_to_end(&mut raw);
    if raw.is_empty() {
        let n = lost(sent.with_context(|| format!("sending to the mcp door at {door}")))?;
        lost(read.with_context(|| format!("reading the mcp door's answer from {door}")))?;
        if n != len {
            bail!("the content ended after {n} of {len} bytes");
        }
    }
    lost(parse_http(&raw))
}

fn parse_http(raw: &[u8]) -> Result<Answer> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").context("the mcp door's answer has no end of headers")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut body = raw[split + 4..].to_vec();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .with_context(|| format!("the mcp door's answer has no status line: {}", head.lines().next().unwrap_or("")))?;
    let header = |name: &str| {
        head.lines()
            .skip(1)
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        body = dechunk(&body)?;
    } else if let Some(n) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        if body.len() < n {
            bail!("the mcp door's answer ended after {} of its {n} bytes", body.len());
        }
        body.truncate(n);
    }
    Ok(Answer { status, body: String::from_utf8_lossy(&body).into_owned() })
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n").context("a chunk with no size line")?;
        let size = std::str::from_utf8(&b[..eol]).ok().and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok()).context("a chunk size that is not hex")?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size {
            bail!("a chunk shorter than its size");
        }
        out.extend_from_slice(&b[..size]);
        b = b.get(size + 2..).unwrap_or(&[]);
    }
}

/// One `tools/call` to the door: the JSON-RPC `result` (a tool result, with
/// `isError` on a refusal), or an error for anything that is not one. A
/// refusal that says the service does not know the call's tool or an
/// argument is annotated with the version skew, when there is one
/// ([`annotate_skew`]).
pub fn call(e: &Environment, door: SocketAddr, tool: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
    let a = post(e, door, &call_body(tool, arguments))?;
    let mut v: serde_json::Value = serde_json::from_str(&a.body).with_context(|| format!("the mcp door answered HTTP {} with no JSON", a.status))?;
    if is_unknown_to_service(&v) {
        if let Some(sv) = service_version(e, door) {
            annotate_skew(&mut v, &sv, &e.name);
        }
    }
    if let Some(r) = v.get("result") {
        return Ok(r.clone());
    }
    bail!("the mcp door answered HTTP {}: {}", a.status, v.get("error").map(|e| e.to_string()).unwrap_or_else(|| v.to_string()))
}

// ------------------------------------------------------------ version skew

/// This cauldron's version: the command line's, the bridge's.
pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// **The running service's version**, as its `initialize` answers it
/// (`serverInfo.version`, which every release of the service has carried).
/// `None` when it cannot be asked.
pub fn service_version(e: &Environment, door: SocketAddr) -> Option<String> {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "cauldron", "version": CLI_VERSION}}})
    .to_string();
    let a = post(e, door, &body).ok()?;
    let v: serde_json::Value = serde_json::from_str(&a.body).ok()?;
    v.pointer("/result/serverInfo/version").and_then(serde_json::Value::as_str).map(str::to_string)
}

/// The two commands that move `env`'s `mcp` service to this checkout's build.
pub fn upgrade_commands(env: &str) -> [String; 2] {
    [
        format!("cauldron --env {env} build {SERVICE} --env-copy"),
        format!("cauldron --env {env} pin {SERVICE} <the label that build printed>"),
    ]
}

/// **What to say when the running service is not this cauldron's version**
/// (`None` when it is): which is which, what that means for a call, and the
/// commands that upgrade it.
pub fn skew_note(service: &str, env: &str) -> Option<String> {
    if service == CLI_VERSION {
        return None;
    }
    let [build, pin] = upgrade_commands(env);
    Some(format!(
        "the `{SERVICE}` service running in environment `{env}` is cauldron {service}, and this cauldron is \
         {CLI_VERSION}: a tool or argument that {service} does not know is refused by it. To run this version's \
         service, run `scripts/install --replace-running` from your checkout, or `{build}` and then `{pin}` (the \
         supervisor moves the running service to the new build)"
    ))
}

/// Does `v` (a JSON-RPC answer from the door) say the service does not know
/// a tool, or an argument, that the call used: what an older service
/// answers a newer client?
pub fn is_unknown_to_service(v: &serde_json::Value) -> bool {
    let refused = v.pointer("/result/structuredContent/refused");
    let usage_arg = refused.is_some_and(|r| {
        r["code"] == "usage" && r["message"].as_str().is_some_and(|m| m.contains(" takes no argument `"))
    });
    let unknown_tool = v.pointer("/error/message").and_then(serde_json::Value::as_str).is_some_and(|m| m.starts_with("unknown tool: "));
    usage_arg || unknown_tool
}

/// Add the skew note to a refusal (`v` a JSON-RPC answer) when the service
/// is another version than this cauldron. Answers whether it did.
pub fn annotate_skew(v: &mut serde_json::Value, service: &str, env: &str) -> bool {
    let Some(note) = skew_note(service, env) else { return false };
    let tail = format!(" Version skew: {note}.");
    if let Some(r) = v.pointer_mut("/result/structuredContent/refused").and_then(serde_json::Value::as_object_mut) {
        if let Some(m) = r.get("message").and_then(serde_json::Value::as_str).map(str::to_string) {
            r.insert("message".into(), serde_json::json!(format!("{m}{tail}")));
        }
        r.insert("version_skew".into(), serde_json::json!({"service": service, "cauldron": CLI_VERSION}));
        if let Some(t) = v.pointer_mut("/result/content/0/text") {
            if let Some(s) = t.as_str().map(str::to_string) {
                *t = serde_json::json!(format!("{s}{tail}"));
            }
        }
        return true;
    }
    if let Some(m) = v.pointer_mut("/error/message") {
        if let Some(s) = m.as_str().map(str::to_string) {
            *m = serde_json::json!(format!("{s}{tail}"));
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An older service's refusal of a newer argument, or of a tool it does
    /// not have, says which versions are running and how to upgrade; the
    /// same refusal from the same version is left as it is.
    #[test]
    fn an_older_service_s_refusal_names_the_skew_and_the_upgrade() {
        let refusal = |m: &str| {
            serde_json::json!({"jsonrpc": "2.0", "id": 3, "result": {"isError": true,
                "content": [{"type": "text", "text": format!("refused (usage): {m}")}],
                "structuredContent": {"refused": {"code": "usage", "message": m}}}})
        };
        let mut v = refusal("soup_dump_get takes no argument `view`. Nothing was read or written.");
        assert!(is_unknown_to_service(&v));
        assert!(annotate_skew(&mut v, "0.0.1", "prod"));
        let m = v.pointer("/result/structuredContent/refused/message").unwrap().as_str().unwrap();
        for want in ["takes no argument `view`", "is cauldron 0.0.1", CLI_VERSION, "scripts/install --replace-running",
                     "cauldron --env prod build mcp --env-copy", "cauldron --env prod pin mcp <the label that build printed>"] {
            assert!(m.contains(want), "{want}: {m}");
        }
        assert!(v.pointer("/result/content/0/text").unwrap().as_str().unwrap().contains("is cauldron 0.0.1"));
        assert_eq!(v.pointer("/result/structuredContent/refused/version_skew/service").unwrap(), "0.0.1");
        // A tool the service does not have at all.
        let mut u = serde_json::json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32602, "message": "unknown tool: soup_dump_get"}});
        assert!(is_unknown_to_service(&u) && annotate_skew(&mut u, "0.0.1", "dev"));
        assert!(u["error"]["message"].as_str().unwrap().contains("cauldron --env dev pin mcp"), "{u}");
        // Same version: untouched. Another refusal: not this kind.
        let mut same = refusal("soup_nodes takes no argument `colour`. Nothing was read or written.");
        let before = same.clone();
        assert!(!annotate_skew(&mut same, CLI_VERSION, "prod"));
        assert_eq!(same, before);
        assert!(!is_unknown_to_service(&refusal("max_chars 5 is under 1000")));
        assert!(skew_note(CLI_VERSION, "prod").is_none());
    }

    #[test]
    fn an_answer_is_read_by_its_length_or_its_chunks() {
        let a = parse_http(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}junk").unwrap();
        assert_eq!((a.status, a.body.as_str()), (200, "{}"));
        let a = parse_http(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4\r\n\":1}\r\n0\r\n\r\n").unwrap();
        assert_eq!(a.body, "{\"a\":1}");
        let a = parse_http(b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n").unwrap();
        assert_eq!((a.status, a.body.as_str()), (202, ""));
        assert!(parse_http(b"garbage").is_err());
    }
}
