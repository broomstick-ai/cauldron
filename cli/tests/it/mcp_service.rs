//! **The `mcp` service beside the command line**: a forwarded import, the
//! refused recover, `cauldron mcp status`, and the stdio bridge.
//!
//! Every test runs the built `cauldron` and the real `cauldron-mcp` (built
//! here from this worktree by cargo, as `environments.rs` builds its probe)
//! under a scratch `CAULDRON_HOME`/`HOME`; this binary exports
//! `CAULDRON_ISOLATED=1`, so a `cauldron` child may dial only the scratch
//! service's listeners, which the test hands it (`crate::grants::owned`).
//!
//! - **F1** while the service holds a granted cauldron, `soup import` is
//!   sent to it and lands through it, printing the import's own JSON (plus
//!   `forwarded_to`); the batch-already-landed guard and `--allow-duplicate`
//!   hold across the two ways of landing; a refused record prints the
//!   refusal as a direct import does; `--dry-run` is never sent;
//!   `soup recover` is refused naming the service; `mcp status` shows it
//!   holding the cauldron. An ungranted cauldron imports directly while the
//!   service runs, and once the service stops, so does the granted one.
//! - **B1** the bridge: with the service down it answers `initialize` and
//!   `tools/list` and refuses every call as `service_down`, naming the start
//!   command; with it up it relays, with `_ctx` set from its own working
//!   directory and session (a caller's own `_ctx` replaced); the door secret
//!   never appears on its stdout or stderr; a service stopped mid-session
//!   turns the next call into `service_down`.
//! - **B5** one server per cauldron: `cauldron mcp config --cauldron <name>`
//!   prints a `.mcp.json` entry whose bridge is pinned to that cauldron, and
//!   that bridge refuses every call naming another (`cauldron_pinned`),
//!   before anything reaches the service.
//! - **D1** dumps: the bridge finds a session's transcript in a fixture
//!   Claude Code store (a scratch `CLAUDE_CONFIG_DIR`, never the real one)
//!   by id, or this session's by `$CLAUDE_CODE_SESSION_ID`, and the stored
//!   blob's hash is the file's; an absent session is `session_unknown`, an
//!   unreadable store or file `transcript_unreadable`, a path `usage`, an
//!   oversized file `too_large`, all before anything is sent. `soup dump`
//!   is sent to the service while it holds the dumps store and lands
//!   directly once it is stopped; `soup dumps` and `soup dump-get` read
//!   them back, `--raw` verbatim.
//! - **D2** the bridge follows no link out of the projects store: a linked
//!   file or project directory, or a second name, is `transcript_link`; a
//!   FIFO or directory in the file's place is `transcript_unreadable`; nothing
//!   reaches the service.
//! - **D3** the dumps store is private whatever the umask: directories
//!   0700, files 0600.
//! - **D7** (only under `--features fault-injection`) a dump removal stopped
//!   after each of its steps reads `removing`, never `removed`, until one
//!   re-run or one writer's open finishes it, after which no byte of the
//!   cauldron holds its text.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use soup_door::grants::Scope;

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");
const README: &str = include_str!("../../../schema/soup/workspace-v6/README.md");

/// `cauldron-mcp`, built from THIS worktree by cargo once per test process,
/// at the path cargo reports.
fn service_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let target = Path::new(CAULDRON).parent().unwrap().parent().unwrap();
        let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "cauldron-mcp", "--bin", "cauldron-mcp", "--message-format=json-render-diagnostics", "--target-dir"])
            .arg(target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stderr(Stdio::inherit())
            .output()
            .expect("spawn cargo build -p cauldron-mcp");
        assert!(out.status.success(), "building cauldron-mcp for the test: {:?}", out.status);
        let exe = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "cauldron-mcp")
            .find_map(|m| m["executable"].as_str().map(PathBuf::from))
            .expect("cargo reported no cauldron-mcp executable");
        assert!(exe.exists(), "cauldron-mcp at {}", exe.display());
        exe
    })
    .clone()
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    fn new(tag: &str, cauldrons: &[&str]) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "cauldron-mcp-svc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        for d in ["root/data/dev/mcp", "home", "work/proj/sub"] {
            std::fs::create_dir_all(p.join(d)).unwrap();
        }
        let s = Scratch(p.canonicalize().unwrap());
        if let Some(h) = std::env::var_os("HOME") {
            assert!(!s.0.starts_with(Path::new(&h).join(".cauldron")), "scratch under ~/.cauldron");
        }
        for c in cauldrons {
            let o = s.run(&["create", c, "--env", "dev"], &[]);
            assert_eq!(o.rc, 0, "create {c}: {}", o.err);
        }
        s
    }
    fn root(&self) -> PathBuf {
        self.0.join("root")
    }
    fn data(&self) -> PathBuf {
        self.root().join("data").join("dev")
    }
    fn mcp_dir(&self) -> PathBuf {
        self.data().join("mcp")
    }
    fn soup(&self, c: &str) -> PathBuf {
        self.data().join("cauldrons").join(c).join("soup")
    }
    fn project(&self) -> PathBuf {
        self.0.join("work").join("proj")
    }
    fn env(&self) -> env::Environment {
        env::Environment {
            root: self.root(),
            root_source: env::paths::Source::CauldronHome,
            name: "dev".into(),
            name_source: env::paths::Source::CauldronEnv,
            data_root: self.data(),
        }
    }
    fn grant(&self, c: &str) {
        let p = soup_door::grants::canonical_project(&self.project()).unwrap();
        soup_door::grants::grant(&self.env(), c, &Scope::ALL, &[p], "tty").unwrap();
    }
    fn command(&self, args: &[&str], pids: &[u32]) -> Command {
        let mut c = Command::new(CAULDRON);
        c.args(args)
            .env("CAULDRON_HOME", self.root())
            .env("HOME", self.0.join("home"))
            .env(crate::grants::ALLOW_ENV, crate::grants::owned(&self.0, pids))
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .env_remove("CAULDRON_CAULDRON")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_CONFIG_DIR")
            .current_dir(self.0.join("work"));
        c
    }
    fn run(&self, args: &[&str], pids: &[u32]) -> Out {
        let o = self.command(args, pids).stdin(Stdio::null()).output().expect("spawn cauldron");
        Out { rc: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() }
    }
    fn import(&self, c: &str, file: &Path, extra: &[&str], pids: &[u32]) -> Out {
        let mut a = vec!["soup", "import", file.to_str().unwrap(), "--env", "dev", "--cauldron", c];
        a.extend_from_slice(extra);
        self.run(&a, pids)
    }
    fn write(&self, name: &str, v: &Value) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, serde_json::to_string(v).unwrap()).unwrap();
        p
    }
    fn node_count(&self, c: &str) -> i64 {
        let conn = spine::offline::read_only_connection(&self.soup(c).join("ws.db")).unwrap();
        conn.query_row("SELECT count(*) FROM nodes", [], |r| r.get(0)).unwrap()
    }
    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.mcp_dir().join("calls.jsonl")).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
    fn secret(&self) -> String {
        std::fs::read_to_string(self.mcp_dir().join("door.secret")).unwrap().trim().to_string()
    }
}

struct Out {
    rc: i32,
    out: String,
    err: String,
}
impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.out).unwrap_or_else(|e| panic!("not JSON ({e}): {}\n{}", self.out, self.err))
    }
}

fn example() -> Vec<Value> {
    let at = README.find("## A worked example").unwrap();
    let start = README[at..].find("```json").unwrap() + at + "```json".len();
    let end = README[start..].find("```").unwrap() + start;
    serde_json::from_str(&README[start..end]).unwrap()
}

fn task(label: &str, handle: &str) -> Value {
    json!({"id": label, "type": "task", "state": "ready", "handle": handle, "edges": [{"kind": "in_region", "to": "R-1"}]})
}

// ------------------------------------------------------------ the service

struct Service {
    child: Child,
    door: SocketAddr,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    /// Started as `cauldron start mcp` starts it.
    fn start(s: &Scratch) -> Service {
        Service::start_bin(s, &service_bin())
    }

    /// `bin` (a `cauldron-mcp`) started as `cauldron start mcp` starts it.
    fn start_bin(s: &Scratch, bin: &Path) -> Service {
        Service::start_bin_with(s, bin, &[])
    }

    /// [`Service::start_bin`] with `vars` added to its environment (what a
    /// service started by hand, not by the supervisor, might carry).
    fn start_bin_with(s: &Scratch, bin: &Path, vars: &[(&str, &str)]) -> Service {
        let log = std::fs::File::create(s.0.join(format!("service-{}.log", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))).unwrap();
        let bin = bin.to_path_buf();
        let child = Command::new(&bin)
            .envs(vars.iter().copied())
            .env("CAULDRON_SERVICE", "mcp")
            .env("CAULDRON_ENV", "dev")
            .env("CAULDRON_HOME", s.root())
            .env("CAULDRON_DATA_ROOT", s.data())
            .env("CAULDRON_SERVICE_DATA_DIR", s.mcp_dir())
            .env("CAULDRON_SEAMS", s.data().join("seams.json"))
            .env("CAULDRON_BOOT_TOKEN", format!("boot-{}", std::process::id()))
            .env("CAULDRON_VERSION", "test")
            .env("CAULDRON_COMMIT", "0000000000000000000000000000000000000000")
            .env("CAULDRON_DIRTY", "0")
            .env("CAULDRON_BIND", "127.0.0.1:0")
            .env("CAULDRON_BINARY", &bin)
            .env("CAULDRON_PROBE", "seam")
            .env("HOME", s.0.join("home"))
            .env_remove("CAULDRON_CAULDRON")
            .env_remove("SPINE_FAULT")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("spawn cauldron-mcp");
        let pid = child.id() as u64;
        let deadline = Instant::now() + Duration::from_secs(60);
        let seam: SocketAddr = loop {
            let t: Value = std::fs::read_to_string(s.data().join("seams.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
            if t["seams"]["mcp"]["pid"].as_u64() == Some(pid) {
                break t["seams"]["mcp"]["addr"].as_str().unwrap().parse().unwrap();
            }
            assert!(Instant::now() < deadline, "the service never registered");
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut c = TcpStream::connect(seam).unwrap();
        writeln!(c, "MCP").unwrap();
        let mut url = String::new();
        BufReader::new(c).read_line(&mut url).unwrap();
        let door = url.trim().strip_prefix("http://").and_then(|u| u.strip_suffix("/mcp")).unwrap().parse().unwrap();
        Service { child, door }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// One `tools/call` straight to the door, as the test's own client.
    fn call(&self, s: &Scratch, tool: &str, mut args: Value) -> Value {
        args["_ctx"] = json!({"client": "test", "cwd": s.project().display().to_string()});
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).to_string();
        let mut c = TcpStream::connect(self.door).unwrap();
        write!(
            c,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.door.port(),
            s.secret(),
            body.len()
        )
        .unwrap();
        let mut raw = String::new();
        c.read_to_string(&mut raw).unwrap();
        let v: Value = serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap();
        v["result"].clone()
    }

    /// SIGTERM, and wait for it to exit.
    fn stop(mut self) {
        Command::new("/bin/kill").args(["-TERM", &self.child.id().to_string()]).status().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the service ignored SIGTERM");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn lock_free(s: &Scratch, c: &str) -> bool {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(s.soup(c).join("writer.lock")).unwrap();
    f.try_lock().is_ok()
}

// --------------------------------------------------------------------- F1

#[test]
fn f1_an_import_is_sent_to_the_service_that_holds_the_soup() {
    let s = Scratch::new("f1", &["kb", "other"]);
    let ex = s.write("example.json", &json!(example()));
    assert_eq!(s.import("kb", &ex, &[], &[]).rc, 0);
    assert_eq!(s.import("other", &ex, &[], &[]).rc, 0);
    // The shape of a direct import's answer, before the service exists.
    let direct = s.import("kb", &s.write("d.json", &json!([task("t", "Direct before the service")])), &["--remint"], &[]);
    assert_eq!(direct.rc, 0, "{}", direct.err);
    let direct_keys: Vec<String> = direct.json().as_object().unwrap().keys().cloned().collect();
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    // Not held until the service's first write.
    assert!(lock_free(&s, "kb"));
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    assert!(!lock_free(&s, "kb"), "the service holds the soup");

    // A real import: sent to the service, landed through it, same shape.
    let before = s.node_count("kb");
    let batch = s.write("batch.json", &json!([task("a", "Forwarded task a"), task("b", "Forwarded task b")]));
    let o = s.import("kb", &batch, &["--remint"], &pids);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["outcome"], "landed", "{d}");
    assert_eq!(d["forwarded_to"], json!({"service": "mcp", "pid": svc.pid()}));
    let mut keys: Vec<String> = d.as_object().unwrap().keys().filter(|k| *k != "forwarded_to").cloned().collect();
    let mut want = direct_keys.clone();
    keys.sort();
    want.sort();
    assert_eq!(keys, want, "the forwarded answer has the direct import's keys");
    assert_eq!(d["judged_on"], "the soup");
    assert!(o.err.contains(&format!("the `mcp` service (pid {})", svc.pid())), "{}", o.err);
    assert_eq!(s.node_count("kb"), before + 2);
    let last = s.calls().into_iter().filter(|l| l["tool"] == "soup_put").last().unwrap();
    assert_eq!((last["client"].clone(), last["outcome"].clone()), (json!("cli"), json!("landed")), "{last}");

    // The same batch again: refused as a direct import refuses it, through
    // the service; --allow-duplicate lands it.
    let o = s.import("kb", &batch, &["--remint"], &pids);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!((d["refusal"].clone(), d["forwarded_to"]["pid"].clone()), (json!("batch-already-landed"), json!(svc.pid())), "{d}");
    assert_eq!(d["landed_before"][0]["records"], 2);
    assert!(o.err.contains("REFUSED (batch-already-landed)") && o.err.contains("--allow-duplicate"), "{}", o.err);
    assert_eq!(s.node_count("kb"), before + 2);
    let o = s.import("kb", &batch, &["--remint", "--allow-duplicate"], &pids);
    assert_eq!((o.rc, o.json()["outcome"].clone()), (0, json!("landed")), "{}", o.err);
    assert_eq!(s.node_count("kb"), before + 4);
    // A batch landed DIRECTLY earlier is known to the service too.
    let o = s.import("kb", &s.write("d.json", &json!([task("t", "Direct before the service")])), &["--remint"], &pids);
    assert_eq!(o.json()["refusal"], "batch-already-landed", "{}", o.out);

    // A refused record: the refusal, printed as a direct import prints it.
    let o = s.import("kb", &s.write("bad.json", &json!([task("x", "Fine"), {"id": "y", "type": "task", "state": "no-such-state", "handle": "Bad state"}])), &["--remint"], &pids);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!((d["outcome"].clone(), d["accepted"].clone(), d["refused"].clone()), (json!("refused"), json!(1), json!(1)), "{d}");
    assert!(d["records"].as_array().unwrap().iter().any(|r| r["id"] == "y" && r["disposition"] == "refused"), "{d}");
    assert_eq!(s.node_count("kb"), before + 4);

    // A dry run is judged on a snapshot, never sent.
    let o = s.import("kb", &s.write("dry.json", &json!([task("t", "Dry")])), &["--remint", "--dry-run"], &pids);
    assert_eq!((o.rc, o.json()["outcome"].clone()), (0, json!("validated")), "{}", o.err);
    assert!(o.json().get("forwarded_to").is_none());

    // recover is refused, naming the service.
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "kb"], &pids);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains(&format!("the `mcp` service (pid {})", svc.pid())) && o.err.contains("cauldron mcp status"), "{}", o.err);

    // status: the service, and what it holds.
    let o = s.run(&["mcp", "status", "--env", "dev"], &pids);
    assert_eq!(o.rc, 0, "{}", o.err);
    let st = o.json();
    assert_eq!((st["service"]["state"].clone(), st["service"]["pid"].clone()), (json!("live"), json!(svc.pid())), "{st}");
    // Its version, beside this cauldron's: the same build, so no skew.
    assert_eq!(st["service"]["version"], st["service"]["cauldron_version"], "{st}");
    assert!(st["service"]["version"].is_string() && st["service"].get("version_skew").is_none(), "{st}");
    assert!(!o.err.contains("VERSION SKEW"), "{}", o.err);
    assert_eq!(st["open"][0]["cauldron"], "kb", "{st}");
    assert_eq!(st["open"][0]["recovered_at_open"]["interrupted"], "none");
    let kb = st["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap();
    assert_eq!((kb["held_by_service"].clone(), kb["writer_lock_says"].clone()), (json!(true), json!(format!("mcp {}", svc.pid()))), "{st}");
    assert_eq!(kb["granted"], json!(["read", "write", "dump"]));
    let other = st["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "other").unwrap();
    assert_eq!((other["held_by_service"].clone(), other["granted"].clone()), (json!(false), Value::Null), "{st}");

    // An ungranted cauldron: the service holds nothing, the import is direct.
    let o = s.import("other", &s.write("o.json", &json!([task("t", "Direct into the ungranted one")])), &["--remint"], &pids);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(o.json().get("forwarded_to").is_none(), "{}", o.out);

    // The service stopped: the import is direct again, and status says so.
    let pid = svc.pid();
    svc.stop();
    assert!(lock_free(&s, "kb"));
    let o = s.import("kb", &s.write("after.json", &json!([task("t", "Direct after the service")])), &["--remint"], &[]);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert!(o.json().get("forwarded_to").is_none(), "{}", o.out);
    assert!(!o.err.contains(&format!("pid {pid}")), "{}", o.err);
    let st = s.run(&["mcp", "status", "--env", "dev"], &[]).json();
    assert_eq!(st["service"]["state"], "not running", "{st}");
    assert_eq!(st["service"]["start"], "cauldron --env dev start mcp");
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "kb"], &[]);
    assert_eq!(o.rc, 0, "recover works again once the service let go: {}", o.err);
}

#[test]
fn f2_an_import_too_large_for_the_door_is_refused_before_it_is_sent() {
    let s = Scratch::new("f2", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    let before = s.node_count("kb");
    let puts = s.calls().into_iter().filter(|l| l["tool"] == "soup_put").count();
    // About 18 MiB of records: more than the door reads in one request.
    let body = "x".repeat(450);
    let records: Vec<Value> = (0..40_000)
        .map(|i| json!({"id": format!("t{i}"), "type": "task", "state": "ready", "handle": format!("Large import task {i}"), "body": body,
            "edges": [{"kind": "in_region", "to": "R-1"}]}))
        .collect();
    let big = s.write("huge.json", &json!(records));
    assert!(std::fs::metadata(&big).unwrap().len() > soup_door::catalogue::REQUEST_BODY_LIMIT as u64);
    let o = s.import("kb", &big, &["--remint"], &pids);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    for want in ["16.0 MiB", "40000 records", "lands all of it or none of it", "Nothing was written", "cauldron mcp revoke kb --env dev", "stop mcp"] {
        assert!(o.err.contains(want), "missing `{want}`: {}", o.err);
    }
    assert!(!o.err.contains("Connection reset"), "{}", o.err);
    assert_eq!(s.node_count("kb"), before, "part of the batch landed");
    assert_eq!(s.calls().into_iter().filter(|l| l["tool"] == "soup_put").count(), puts, "the batch was sent");
}

// --------------------------------------------------------------------- B1

/// A bridge, driven line by line as Claude Code drives it.
struct Bridge {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    err: PathBuf,
    seen: String,
}

impl Bridge {
    fn start(s: &Scratch, cwd: &Path, extra: &[&str], vars: &[(&str, &str)], pids: &[u32]) -> Bridge {
        let mut a = vec!["--env", "dev"];
        a.extend_from_slice(extra);
        Bridge::start_bare(s, cwd, &a, vars, pids)
    }

    /// `cauldron mcp stdio <args>`, with no `--env` added.
    fn start_bare(s: &Scratch, cwd: &Path, args: &[&str], vars: &[(&str, &str)], pids: &[u32]) -> Bridge {
        let err = s.0.join(format!("bridge-{}.err", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let mut a = vec!["mcp", "stdio"];
        a.extend_from_slice(args);
        let mut c = s.command(&a, pids);
        c.current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(std::fs::File::create(&err).unwrap());
        for (k, v) in vars {
            c.env(k, v);
        }
        let mut child = c.spawn().expect("spawn the bridge");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Bridge { child, stdin, stdout, err, seen: String::new() }
    }

    /// Send one message; read one answer line (None for a notification).
    fn ask(&mut self, msg: Value) -> Option<Value> {
        let w = self.stdin.as_mut().unwrap();
        writeln!(w, "{msg}").unwrap();
        w.flush().unwrap();
        msg.get("id")?;
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        self.seen.push_str(&line);
        let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("not one JSON line ({e}): {line}"));
        assert_eq!(v["id"], msg["id"], "{v}");
        Some(v)
    }

    fn call(&mut self, id: u64, tool: &str, args: Value) -> Value {
        self.ask(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}})).unwrap()["result"].clone()
    }

    /// Close stdin; the bridge exits; everything it wrote.
    fn finish(mut self) -> (i32, String, String) {
        drop(self.stdin.take());
        let mut rest = String::new();
        self.stdout.read_to_string(&mut rest).unwrap();
        self.seen.push_str(&rest);
        let st = self.child.wait().unwrap();
        (st.code().unwrap_or(-1), self.seen.clone(), std::fs::read_to_string(&self.err).unwrap_or_default())
    }
}

fn init(b: &mut Bridge) -> Value {
    let i = b.ask(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}})).unwrap();
    assert!(b.ask(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
    i
}

fn refused(r: &Value) -> (String, String) {
    assert_eq!(r["isError"], json!(true), "not refused: {r}");
    let x = &r["structuredContent"]["refused"];
    (x["code"].as_str().unwrap().to_string(), x["message"].as_str().unwrap().to_string())
}

#[test]
fn b1_the_bridge_relays_with_its_own_context_and_answers_when_the_service_is_down() {
    let s = Scratch::new("b1", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");
    let names = soup_door::catalogue::TOOLS.iter().map(|t| t.name).collect::<Vec<_>>();
    let sub = s.project().join("sub");

    // DOWN: initialize and tools/list answered by the bridge; every call
    // refused as service_down, naming the start command.
    let mut b = Bridge::start(&s, &sub, &[], &[], &[]);
    let i = init(&mut b);
    assert_eq!(i["result"]["protocolVersion"], "2025-06-18", "{i}");
    assert!(i["result"]["instructions"].as_str().unwrap().contains("cauldron --env dev start mcp"), "{i}");
    let list = b.ask(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    let listed: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(listed, names);
    let (code, msg) = refused(&b.call(2, "soup_status", json!({})));
    assert_eq!(code, "service_down");
    assert!(msg.contains("`cauldron --env dev start mcp`"), "{msg}");
    let (rc, _, err) = b.finish();
    assert_eq!(rc, 0, "{err}");

    // UP: relayed, with the bridge's own context.
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let secret = s.secret();
    let mut b = Bridge::start(&s, &sub, &[], &[("CLAUDE_CODE_SESSION_ID", "sess-b1")], &pids);
    let i = init(&mut b);
    assert!(!i["result"]["instructions"].as_str().unwrap().contains("not running"), "{i}");
    assert!(!i["result"]["instructions"].as_str().unwrap().contains("VERSION SKEW"), "the same build: no skew: {i}");
    let list = b.ask(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), names.len());
    assert!(list["result"]["tools"].as_array().unwrap().iter().all(|t| t["inputSchema"]["properties"].get("_ctx").is_none()));
    // The project is found from the bridge's working directory (a
    // subdirectory of the granted project), whatever `_ctx` the caller sent.
    let st = b.call(2, "soup_status", json!({"_ctx": {"client": "evil", "cwd": "/", "session_id": "forged"}}));
    assert_eq!(st["isError"], json!(false), "{st}");
    let want = format!("project:{}", s.project().display());
    assert_eq!(st["structuredContent"]["resolved_from"], json!(want), "{st}");
    // A write: the session from the bridge's environment is cited and audited.
    let put = b.call(3, "soup_put", json!({"remint": true, "records": [task("t", "Through the bridge")], "_ctx": {"client": "evil"}}));
    assert_eq!(put["isError"], json!(false), "{put}");
    let id = put["structuredContent"]["id_map"]["t"].as_str().unwrap().to_string();
    let got = b.call(4, "soup_get", json!({"ids": [id]}));
    let cites = &got["structuredContent"]["records"][0]["meta"]["cites"];
    assert!(cites.as_array().unwrap().contains(&json!({"kind": "session", "id": "sess-b1"})), "{got}");
    let line = s.calls().into_iter().filter(|l| l["tool"] == "soup_put").last().unwrap();
    assert_eq!((line["client"].clone(), line["session"].clone()), (json!("claude-code"), json!("sess-b1")), "{line}");
    assert!(s.calls().iter().all(|l| l["client"] != "evil" && l["session"] != "forged"));
    // The service's own refusals reach the caller as they are.
    let (code, _) = refused(&b.call(5, "soup_get", json!({"ids": "R-1"})));
    assert_eq!(code, "usage");
    let unknown = b.ask(json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "soup_grant", "arguments": {}}})).unwrap();
    assert_eq!(unknown["error"]["code"], json!(-32602), "{unknown}");
    // Stopped mid-session: the next call is service_down.
    svc.stop();
    let (code, _) = refused(&b.call(7, "soup_status", json!({})));
    assert_eq!(code, "service_down");
    let (rc, out, err) = b.finish();
    assert_eq!(rc, 0, "{err}");
    assert!(!out.contains(&secret) && !err.contains(&secret), "the door secret reached the bridge's output");

    // --cauldron names the cauldron outside any granted project.
    let svc = Service::start(&s);
    let secret2 = s.secret();
    let mut b = Bridge::start(&s, &s.0.join("work"), &["--cauldron", "kb"], &[], &[svc.pid()]);
    init(&mut b);
    let st = b.call(1, "soup_status", json!({}));
    assert_eq!(st["structuredContent"]["resolved_from"], "CAULDRON_CAULDRON", "{st}");
    let (_, out, err) = b.finish();
    assert!(!out.contains(&secret2) && !err.contains(&secret2));
    let mut b = Bridge::start(&s, &s.0.join("work"), &[], &[], &[svc.pid()]);
    init(&mut b);
    let (code, _) = refused(&b.call(1, "soup_status", json!({})));
    assert_eq!(code, "no_cauldron_for_project");
    b.finish();
    svc.stop();
}

#[test]
fn b2_the_bridge_does_not_read_env_and_answers_when_its_environment_cannot_be_used() {
    let s = Scratch::new("b2", &["kb"]);
    let cwd = s.0.join("work");
    // `$ENV` names the scratch's real environment, `dev`; the bridge ignores
    // it and looks for `prod`, which this root does not have. It still
    // starts, lists every tool and says why each call is refused.
    let mut b = Bridge::start_bare(&s, &cwd, &[], &[("ENV", "dev")], &[]);
    let i = init(&mut b);
    assert!(i["result"]["instructions"].as_str().unwrap().contains("$ENV is not read"), "{i}");
    let list = b.ask(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), soup_door::catalogue::TOOLS.len(), "{list}");
    let (code, msg) = refused(&b.call(2, "soup_status", json!({"cauldron": "kb"})));
    assert_eq!(code, "no_environment", "{msg}");
    assert!(msg.contains("environment `prod`") && msg.contains("$CAULDRON_ENV") && msg.contains("$ENV is not read"), "{msg}");
    let (rc, _, err) = b.finish();
    assert_eq!(rc, 0, "{err}");
    // A common `ENV=Development` (not even a usable name) does not stop a
    // bridge whose environment is named: the service is down, and it says so.
    let mut b = Bridge::start_bare(&s, &cwd, &[], &[("ENV", "Development"), ("CAULDRON_ENV", "dev")], &[]);
    init(&mut b);
    let (code, msg) = refused(&b.call(1, "soup_status", json!({"cauldron": "kb"})));
    assert_eq!(code, "service_down", "{msg}");
    assert!(msg.contains("environment `dev`"), "{msg}");
    assert_eq!(b.finish().0, 0);
}

// --------------------------------------------------------------------- D1

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(b))
}

/// A fixture transcript: `n` JSONL lines recording `cwd`.
fn transcript(n: usize, cwd: &str, word: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        out.extend_from_slice(json!({"type": "user", "cwd": cwd, "message": {"role": "user", "content": format!("{word} {i}")}}).to_string().as_bytes());
        out.push(b'\n');
    }
    out
}

fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn d1_sessions_and_text_are_dumped_through_the_bridge_and_the_command_line() {
    let s = Scratch::new("d1", &["kb"]);
    s.grant("kb");
    // A fixture Claude Code store, under the scratch root.
    let config = s.0.join("claude-config");
    let a_dir = config.join("projects").join("-work-a");
    let b_dir = config.join("projects").join("-work-b");
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();
    let ta = transcript(300, "/work/a", "alpha");
    let tb = transcript(20, "/work/b", "beta");
    std::fs::write(a_dir.join("sess-a.jsonl"), &ta).unwrap();
    std::fs::write(b_dir.join("sess-b.jsonl"), &tb).unwrap();
    let cfg = config.display().to_string();
    let vars = [("CLAUDE_CONFIG_DIR", cfg.as_str()), ("CLAUDE_CODE_SESSION_ID", "sess-b")];

    // The service down: refused before anything is read.
    let mut b = Bridge::start(&s, &s.project(), &[], &vars, &[]);
    init(&mut b);
    assert_eq!(refused(&b.call(1, "soup_dump_session", json!({"session_id": "sess-a"}))).0, "service_down");
    b.finish();

    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let mut b = Bridge::start(&s, &s.project(), &[], &vars, &pids);
    init(&mut b);
    // By id.
    let r = b.call(2, "soup_dump_session", json!({"session_id": "sess-a", "title": "Session A"}));
    assert_eq!(r["isError"], json!(false), "{r}");
    let d = &r["structuredContent"];
    assert_eq!((d["dump_id"].clone(), d["state"].clone(), d["kind"].clone()), (json!("DM-1"), json!("received"), json!("session")), "{d}");
    assert_eq!(d["sha256"], json!(sha256_hex(&ta)), "the blob's hash is the file's");
    assert_eq!((d["lines"].clone(), d["cwd"].clone(), d["session_id"].clone()), (json!(300), json!("/work/a"), json!("sess-a")));
    assert_eq!(d["resolved_from"], json!(format!("project:{}", s.project().display())));
    // This session, from the bridge's environment.
    let r = b.call(3, "soup_dump_session", json!({}));
    assert_eq!(r["structuredContent"]["session_id"], "sess-b", "{r}");
    assert_eq!(r["structuredContent"]["sha256"], json!(sha256_hex(&tb)));
    // Absent; a path; unreadable; too large: each refused, nothing sent.
    let (code, msg) = refused(&b.call(4, "soup_dump_session", json!({"session_id": "sess-none"})));
    assert_eq!(code, "session_unknown", "{msg}");
    let (code, _) = refused(&b.call(5, "soup_dump_session", json!({"session_id": "../projects/-work-a/sess-a"})));
    assert_eq!(code, "usage");
    let u_dir = config.join("projects").join("-work-u");
    std::fs::create_dir_all(&u_dir).unwrap();
    std::fs::write(u_dir.join("sess-u.jsonl"), b"{}\n").unwrap();
    chmod(&u_dir.join("sess-u.jsonl"), 0o000);
    let (code, msg) = refused(&b.call(6, "soup_dump_session", json!({"session_id": "sess-u"})));
    assert_eq!(code, "transcript_unreadable", "an unreadable file is not absent: {msg}");
    chmod(&u_dir.join("sess-u.jsonl"), 0o600);
    chmod(&u_dir, 0o000);
    let (code, msg) = refused(&b.call(7, "soup_dump_session", json!({"session_id": "sess-u"})));
    assert_eq!(code, "transcript_unreadable", "an unreadable store is not absent: {msg}");
    // A readable copy elsewhere still wins over a locked sibling.
    assert_eq!(b.call(8, "soup_dump_session", json!({"session_id": "sess-a"}))["isError"], json!(false));
    chmod(&u_dir, 0o700);
    let big = a_dir.join("sess-big.jsonl");
    std::fs::File::create(&big).unwrap().set_len(soup_door::dumps::TRANSCRIPT_LIMIT + 1).unwrap();
    let (code, _) = refused(&b.call(9, "soup_dump_session", json!({"session_id": "sess-big"})));
    assert_eq!(code, "too_large");
    // Unknown key, through the service.
    let (code, _) = refused(&b.call(10, "soup_dump_session", json!({"session_id": "sess-a", "path": "/etc/hosts"})));
    assert_eq!(code, "usage");
    // A text dump through the bridge.
    let r = b.call(11, "soup_dump", json!({"text": "from the model", "title": "t"}));
    assert_eq!(r["structuredContent"]["dump_id"], "DM-4", "{r}");
    b.finish();
    let landed: Vec<Value> = s.calls().into_iter().filter(|l| l["tool"] == "soup_dump_session").collect();
    assert_eq!(landed.len(), 4, "only the calls that reached the service are audited: {landed:?}");
    assert!(landed.iter().all(|l| l["session"] == "sess-b" && l["client"] == "claude-code"));

    // The command line, while the service holds the dumps store: sent to it.
    let note = s.0.join("note.txt");
    std::fs::write(&note, "a note from the shell\n").unwrap();
    let o = s.run(&["soup", "dump", note.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--title", "Shell note"], &pids);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let j = o.json();
    assert_eq!((j["dump_id"].clone(), j["forwarded_to"]["pid"].clone()), (json!("DM-5"), json!(svc.pid())), "{j}");
    assert!(s.calls().iter().any(|l| l["tool"] == "soup_dump" && l["client"] == "cli"));
    let list = s.run(&["soup", "dumps", "--env", "dev", "--cauldron", "kb"], &pids).json();
    assert_eq!(list["total"], 5, "{list}");
    assert!(!list.to_string().contains("alpha"), "a list never holds content");
    let raw = s.command(&["soup", "dump-get", "DM-1", "--env", "dev", "--cauldron", "kb", "--raw"], &pids).stdin(Stdio::null()).output().unwrap();
    assert_eq!(raw.stdout, ta, "read back verbatim");
    let w = s.run(&["soup", "dump-get", "DM-1", "--env", "dev", "--cauldron", "kb", "--max-chars", "2000"], &pids).json();
    assert!(serde_json::to_string(&w).unwrap().chars().count() <= 2000);
    assert!(w["next_offset"].as_u64().is_some(), "{w}");
    // status: the dumps store beside the soup, held by the service, counted.
    let st = s.run(&["mcp", "status", "--env", "dev"], &pids).json();
    assert_eq!(st["open_dumps"][0]["cauldron"], "kb", "{st}");
    let kb = st["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!(kb["dumps"], json!({"store": "present", "total": 5, "by_state": {"received": 5}, "held_by_service": true}), "{st}");
    svc.stop();
    let st = s.run(&["mcp", "status", "--env", "dev"], &[]).json();
    let kb = st["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!((kb["dumps"]["held_by_service"].clone(), kb["dumps"]["total"].clone()), (json!(false), json!(5)), "{st}");

    // Stopped: the dumps lock is free, the command line writes directly.
    let o = s.command(&["soup", "dump", "-", "--env", "dev", "--cauldron", "kb"], &[]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut o = o;
    o.stdin.take().unwrap().write_all(b"from stdin").unwrap();
    let o = o.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let j: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["dump_id"], "DM-6");
    assert!(j.get("forwarded_to").is_none());
    let huge = s.0.join("huge.txt");
    std::fs::write(&huge, "y".repeat(soup_door::dumps::TEXT_LIMIT + 1)).unwrap();
    let o = s.run(&["soup", "dump", huge.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"], &[]);
    assert_eq!(o.rc, 1);
    assert!(o.err.contains("too_large"), "{}", o.err);
    // The soup was never touched by any of it.
    assert_eq!(s.node_count("kb"), 0);
    assert!(spine::offline::rest_check(&s.soup("kb")).unwrap().is_empty());
}

/// No link is followed under the projects store: a linked transcript file,
/// a linked project directory and a file with a second name are refused as
/// `transcript_link`, a FIFO or a directory in the file's place as
/// `transcript_unreadable`, each before anything is read or sent; a plain
/// copy beside them does not make any of them usable.
#[test]
fn d2_the_bridge_follows_no_link_out_of_the_projects_store() {
    let s = Scratch::new("d2", &["kb"]);
    s.grant("kb");
    let config = s.0.join("claude-config");
    let projects = config.join("projects");
    let plain = projects.join("-work-plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("sess-ok.jsonl"), transcript(5, "/work/plain", "fine")).unwrap();
    // Outside the store: a file the user can read, and a directory holding
    // a file named as a transcript.
    let outside = s.0.join("outside");
    std::fs::create_dir_all(outside.join("dir")).unwrap();
    std::fs::write(outside.join("private.txt"), "OUTSIDE-THE-STORE\n").unwrap();
    std::fs::write(outside.join("dir").join("sess-dir.jsonl"), "OUTSIDE-THE-STORE\n").unwrap();
    let evil = projects.join("-evil");
    std::fs::create_dir_all(&evil).unwrap();
    std::os::unix::fs::symlink(outside.join("private.txt"), evil.join("sess-file.jsonl")).unwrap();
    std::os::unix::fs::symlink(outside.join("dir"), projects.join("-linkdir")).unwrap();
    std::fs::hard_link(outside.join("private.txt"), evil.join("sess-hard.jsonl")).unwrap();
    let fifo = evil.join("sess-fifo.jsonl");
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    std::fs::create_dir_all(evil.join("sess-subdir.jsonl")).unwrap();
    // The same session as a plain file AND a link: the link refuses.
    std::os::unix::fs::symlink(outside.join("private.txt"), evil.join("sess-ok.jsonl")).unwrap();

    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let cfg = config.display().to_string();
    let mut b = Bridge::start(&s, &s.project(), &[], &[("CLAUDE_CONFIG_DIR", cfg.as_str())], &pids);
    init(&mut b);
    let mut id = 1;
    for (session, want) in [
        ("sess-file", "transcript_link"),
        ("sess-dir", "transcript_link"),
        ("sess-hard", "transcript_link"),
        ("sess-ok", "transcript_link"),
        ("sess-fifo", "transcript_unreadable"),
        ("sess-subdir", "transcript_unreadable"),
    ] {
        id += 1;
        let (code, msg) = refused(&b.call(id, "soup_dump_session", json!({"session_id": session})));
        assert_eq!(code, want, "{session}: {msg}");
        assert!(msg.contains("Nothing was read or written"), "{session}: {msg}");
        if want == "transcript_link" {
            assert!(msg.contains("a link was found"), "{session}: {msg}");
        }
    }
    b.finish();
    assert_eq!(s.calls().into_iter().filter(|l| l["tool"] == "soup_dump_session").count(), 0, "nothing reached the service");
    assert!(!s.data().join("cauldrons").join("kb").join("dumps").exists(), "no store was made, no byte stored");
    svc.stop();
}

/// Under a umask that takes nothing away, a dump made by the command line
/// still leaves the dumps store private: directories 0700, files 0600.
#[test]
fn d3_the_dumps_store_is_private_whatever_the_umask() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("d3", &["kb"]);
    let base = s.command(&["soup", "dump", "-", "--env", "dev", "--cauldron", "kb"], &[]);
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg("umask 000; exec \"$0\" \"$@\"").arg(base.get_program()).args(base.get_args());
    for (k, v) in base.get_envs() {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    c.current_dir(base.get_current_dir().unwrap()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"words for the owner only").unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let d = s.data().join("cauldrons").join("kb").join("dumps");
    let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&d), 0o700);
    assert_eq!(mode(&d.join("blobs")), 0o700);
    let mut files = 0;
    for dir in [d.clone(), d.join("blobs")] {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            if e.file_type().unwrap().is_file() {
                files += 1;
                assert_eq!(mode(&e.path()), 0o600, "{}", e.path().display());
            }
        }
    }
    assert!(files >= 4, "spine.db, dumps.db, writer.lock and the blob: {files}");
}

// --------------------------------------------------------------------- D4

/// **Many dumps at once, through the service and the command line, never
/// collide.** While the service holds the dumps store, 8 clients send 15
/// `soup_dump` calls each straight to its door and 12 `cauldron soup dump`
/// commands run at the same time (each sent on to the service): half the
/// texts are one and the same, half are each their own. Every call lands
/// (0.1.7 refused some as `store`, `File exists`, when two temporary blob
/// names met); the ids are DM-1..DM-132, each once; the identical texts share
/// one blob; every dump reads back as the text that was sent.
#[test]
fn d4_concurrent_dumps_through_the_service_and_the_command_line_all_land() {
    let s = std::sync::Arc::new(Scratch::new("d4", &["kb"]));
    s.grant("kb");
    let svc = std::sync::Arc::new(Service::start(&s));
    let pids = [svc.pid()];
    let same = "the same words, sent by everyone at once\n";
    let own = |who: &str, n: usize| format!("words of {who}, number {n}, theirs alone\n");
    // The store held by the service before the rush.
    let first = svc.call(&s, "soup_dump", json!({"cauldron": "kb", "text": same}));
    assert_eq!(first["structuredContent"]["dump_id"], "DM-1", "{first}");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8 + 12));
    let mut handles = Vec::new();
    for t in 0..8 {
        let (s, svc, barrier) = (s.clone(), svc.clone(), barrier.clone());
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let mut got = Vec::new();
            for n in 0..15 {
                let text = if n % 2 == 0 { same.to_string() } else { own(&format!("door {t}"), n) };
                let r = svc.call(&s, "soup_dump", json!({"cauldron": "kb", "text": text}));
                got.push((r, text));
            }
            got
        }));
    }
    for t in 0..12 {
        let (s, barrier) = (s.clone(), barrier.clone());
        handles.push(std::thread::spawn(move || {
            let text = if t % 2 == 0 { same.to_string() } else { own(&format!("shell {t}"), 0) };
            let mut c = s.command(&["soup", "dump", "-", "--env", "dev", "--cauldron", "kb"], &pids);
            c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
            barrier.wait();
            let mut child = c.spawn().unwrap();
            child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
            let o = child.wait_with_output().unwrap();
            assert!(o.status.success(), "a shell dump failed: {}", String::from_utf8_lossy(&o.stderr));
            let j: Value = serde_json::from_slice(&o.stdout).unwrap();
            assert_eq!(j["forwarded_to"]["pid"], json!(pids[0]), "{j}");
            vec![(json!({"isError": false, "structuredContent": j}), text)]
        }));
    }
    let mut landed: Vec<(String, String)> = Vec::new();
    for h in handles {
        for (r, text) in h.join().unwrap() {
            assert_eq!(r["isError"], json!(false), "a dump was refused: {r}");
            landed.push((r["structuredContent"]["dump_id"].as_str().unwrap().to_string(), text));
        }
    }
    assert_eq!(landed.len(), 8 * 15 + 12);
    let mut ids: Vec<u64> = landed.iter().map(|(id, _)| id.strip_prefix("DM-").unwrap().parse().unwrap()).collect();
    ids.sort();
    assert_eq!(ids, (2..=(1 + 8 * 15 + 12) as u64).collect::<Vec<_>>(), "every id once");
    // One blob per distinct text.
    let distinct: std::collections::HashSet<&str> = landed.iter().map(|(_, t)| t.as_str()).chain([same]).collect();
    let blobs = std::fs::read_dir(s.data().join("cauldrons/kb/dumps/blobs")).unwrap().flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.')).count();
    assert_eq!(blobs, distinct.len(), "the identical texts share one blob, and nothing else is left");
    // Each reads back as what was sent.
    for (id, text) in landed.iter().step_by(7) {
        let g = svc.call(&s, "soup_dump_get", json!({"cauldron": "kb", "id": id}));
        assert_eq!(g["structuredContent"]["text"].as_str(), Some(text.as_str()), "{id}: {g}");
    }
    let list = s.run(&["soup", "dumps", "--env", "dev", "--cauldron", "kb"], &pids).json();
    assert_eq!(list["total"], json!(1 + 8 * 15 + 12), "{list}");
}

/// Every file under `root` that holds `needle`, by path.
fn files_holding(root: &Path, needle: &str) -> Vec<PathBuf> {
    let mut hits = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else if std::fs::read(e.path()).unwrap().windows(needle.len()).any(|w| w == needle.as_bytes()) {
                hits.push(e.path());
            }
        }
    }
    hits
}

/// `soup dump-remove`: refused while the service holds the dumps store (and
/// never sent to it), refused without its flags, a dry run that changes
/// nothing (exit 2), then the removal: the planted text is in no file of
/// the cauldron, and the dump reads back as a tombstone, from the command
/// line and through the door.
#[test]
fn d5_a_removed_dump_leaves_no_byte_and_reads_as_a_tombstone() {
    let s = Scratch::new("d5", &["kb"]);
    s.grant("kb");
    let marker = "PLANTED-DUMP-MARKER-WIBBLE";
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_dump", json!({"cauldron": "kb", "text": format!("token {marker}\nmore"), "title": format!("t {marker}")}));
    assert_eq!(r["structuredContent"]["dump_id"], "DM-1", "{r}");
    let r = svc.call(&s, "soup_dump", json!({"cauldron": "kb", "text": "nothing secret here"}));
    assert_eq!(r["structuredContent"]["dump_id"], "DM-2", "{r}");
    let cauldron_dir = s.data().join("cauldrons").join("kb");
    assert!(!files_holding(&cauldron_dir, marker).is_empty(), "planted");

    let o = s.run(&["soup", "dump-remove", "DM-1", "--env", "dev", "--cauldron", "kb", "--yes"], &pids);
    assert_eq!(o.rc, 1, "{}", o.out);
    assert!(o.err.contains("holds cauldron `kb`'s dumps store") && o.err.contains("cauldron mcp revoke kb --env dev"), "{}", o.err);
    assert!(!s.calls().iter().any(|l| l["tool"].as_str().is_some_and(|t| t.contains("remove"))), "nothing was sent to the service");
    svc.stop();

    let o = s.run(&["soup", "dump-remove", "DM-1", "--cauldron", "kb", "--yes"], &[]);
    assert_eq!(o.rc, 1);
    assert!(o.err.contains("only as flags"), "{}", o.err);
    let o = s.run(&["soup", "dump-remove", "DM-1", "--env", "dev", "--cauldron", "kb"], &[]);
    assert_eq!(o.rc, 2, "{}", o.err);
    assert_eq!(o.json()["dry_run"], true);
    assert!(!files_holding(&cauldron_dir, marker).is_empty(), "a dry run changes nothing");

    let o = s.run(&["soup", "dump-remove", "DM-1", "--env", "dev", "--cauldron", "kb", "--yes"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let j = o.json();
    assert_eq!((j["state"].clone(), j["blob"].clone()), (json!("removed"), json!("deleted")), "{j}");
    let left = files_holding(&cauldron_dir, marker);
    assert!(left.is_empty(), "left behind: {left:?}");

    let g = s.run(&["soup", "dump-get", "DM-1", "--env", "dev", "--cauldron", "kb"], &[]).json();
    assert_eq!((g["removed"].clone(), g["dump"]["state"].clone()), (json!(true), json!("removed")), "{g}");
    let raw = s.run(&["soup", "dump-get", "DM-1", "--env", "dev", "--cauldron", "kb", "--raw"], &[]);
    assert_eq!(raw.rc, 1);
    assert!(raw.err.contains("was removed"), "{}", raw.err);
    // Through the door, after the service opens the store again.
    let svc = Service::start(&s);
    let g = svc.call(&s, "soup_dump_get", json!({"cauldron": "kb", "id": "DM-1"}));
    assert_eq!((g["isError"].clone(), g["structuredContent"]["dump"]["state"].clone()), (json!(false), json!("removed")), "{g}");
    let g = svc.call(&s, "soup_dump_get", json!({"cauldron": "kb", "id": "DM-2"}));
    assert_eq!(g["structuredContent"]["text"], "nothing secret here", "{g}");
    let r = svc.call(&s, "soup_dump", json!({"cauldron": "kb", "text": "after"}));
    assert_eq!(r["structuredContent"]["dump_id"], "DM-3", "{r}");
    svc.stop();
}

/// Every step a removal can be stopped after, in order (the `dumps.db` and
/// `spine.db` steps are each database file's compaction).
#[cfg(feature = "fault-injection")]
const REMOVAL_STEPS: [&str; 11] = [
    "marked",
    "blob-deleted",
    "history-erased",
    "checkpointed:dumps.db",
    "vacuumed:dumps.db",
    "compacted:dumps.db",
    "checkpointed:spine.db",
    "vacuumed:spine.db",
    "compacted:spine.db",
    "erased",
    "recorded",
];

/// A cauldron holding DM-1 (text and title carrying `marker`) and DM-2.
#[cfg(feature = "fault-injection")]
fn two_dumps(tag: &str, marker: &str) -> Scratch {
    let s = Scratch::new(tag, &["kb"]);
    let f = s.0.join("note.txt");
    std::fs::write(&f, format!("line with {marker}\n").repeat(200)).unwrap();
    let title = format!("title {marker}");
    let o = s.run(&["soup", "dump", f.to_str().unwrap(), "--env", "dev", "--cauldron", "kb", "--title", &title], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    std::fs::write(&f, "a note that stays").unwrap();
    let o = s.run(&["soup", "dump", f.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    s
}

/// `soup dump-remove DM-1`, stopped by the removal's seam right after `step`.
#[cfg(feature = "fault-injection")]
fn remove_stopped_after(s: &Scratch, step: &str) -> Out {
    let o = s
        .command(&["soup", "dump-remove", "DM-1", "--env", "dev", "--cauldron", "kb", "--yes"], &[])
        .env("CAULDRON_REMOVE_FAULT", step)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Out { rc: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() }
}

#[cfg(feature = "fault-injection")]
fn dump_state(s: &Scratch, id: &str) -> Value {
    let o = s.run(&["soup", "dump-get", id, "--env", "dev", "--cauldron", "kb"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    o.json()
}

/// **D7** a removal stopped after EVERY one of its steps (the mark, the
/// blob, the rewrite of the record, each database file's checkpoint, VACUUM
/// and last checkpoint, the end of the erasure, and the final commit; the
/// seam aborts the process there, as a kill would): until it is finished
/// the dump reads `removing` with `erasure: unfinished` and the command that
/// finishes it, never `removed`; after ONE re-run of the removal, or ONE
/// open of the store by a writer (a `soup dump` from the command line, or
/// the `mcp` service's first dump), the planted text is in no byte of the
/// cauldron's directory and the dump reads `removed`. A finish that is
/// itself stopped is finished by the next open.
#[cfg(feature = "fault-injection")]
#[test]
fn d7_a_removal_stopped_after_any_step_says_unfinished_and_one_rerun_or_reopen_finishes_it() {
    let cmd = "cauldron soup dump-remove DM-1 --env dev --cauldron kb --yes";
    for (i, step) in REMOVAL_STEPS.iter().enumerate() {
        for finish in ["rerun", "reopen"] {
            let marker = format!("PLANTED-STOPPED-REMOVAL-{i}-{}", if finish == "rerun" { "RR" } else { "RO" });
            let s = two_dumps(&format!("d7-{i}-{finish}"), &marker);
            let dir = s.data().join("cauldrons").join("kb");
            assert!(!files_holding(&dir, &marker).is_empty(), "planted");
            let o = remove_stopped_after(&s, step);
            assert_ne!(o.rc, 0, "{step}: the seam did not stop the removal: {}", o.out);
            assert!(o.err.contains(&format!("aborting after `{step}`")), "{step}: {}", o.err);
            let g = dump_state(&s, "DM-1");
            if *step == "recorded" {
                assert_eq!((g["dump"]["state"].clone(), g["erasure"].clone()), (json!("removed"), json!("finished")), "{step}: {g}");
                let left = files_holding(&dir, &marker);
                assert!(left.is_empty(), "{step}: `removed` recorded with text left: {left:?}");
            } else {
                assert_eq!((g["dump"]["state"].clone(), g["erasure"].clone()), (json!("removing"), json!("unfinished")), "{step}: {g}");
                assert_eq!(g["finish_with"], cmd, "{step}: {g}");
                assert_eq!(g["text"], "", "{step}: no read answers the content");
                let l = s.run(&["soup", "dumps", "--env", "dev", "--cauldron", "kb"], &[]).json();
                assert_eq!(l["dumps"][1]["state"], "removing", "{step}: {l}");
            }
            match finish {
                "rerun" => {
                    let o = s.run(&["soup", "dump-remove", "DM-1", "--env", "dev", "--cauldron", "kb", "--yes"], &[]);
                    assert_eq!(o.rc, 0, "{step}: {}", o.err);
                    assert_eq!(o.json()["state"], "removed", "{step}");
                }
                _ => {
                    let f = s.0.join("after.txt");
                    std::fs::write(&f, "written after the stop").unwrap();
                    let o = s.run(&["soup", "dump", f.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"], &[]);
                    assert_eq!(o.rc, 0, "{step}: {}", o.err);
                    assert_eq!(o.json()["dump_id"], "DM-3", "{step}");
                }
            }
            let left = files_holding(&dir, &marker);
            assert!(left.is_empty(), "{step}/{finish}: left behind: {left:?}");
            let g = dump_state(&s, "DM-1");
            assert_eq!((g["dump"]["state"].clone(), g["erasure"].clone()), (json!("removed"), json!("finished")), "{step}/{finish}: {g}");
            assert_eq!(dump_state(&s, "DM-2")["text"], "a note that stays", "{step}/{finish}");
        }
    }

    // The finish at an open, itself stopped mid-way, then finished by the
    // next open: the `mcp` service's, on its first dump.
    let marker = "PLANTED-STOPPED-TWICE";
    let s = two_dumps("d7-twice", marker);
    let dir = s.data().join("cauldrons").join("kb");
    assert_ne!(remove_stopped_after(&s, "marked").rc, 0);
    let f = s.0.join("after.txt");
    std::fs::write(&f, "written after the stop").unwrap();
    let o = s
        .command(&["soup", "dump", f.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"], &[])
        .env("CAULDRON_REMOVE_FAULT", "vacuumed:spine.db")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_ne!(o.status.code(), Some(0), "the open's finish was not stopped");
    assert_eq!(dump_state(&s, "DM-1")["dump"]["state"], "removing");
    s.grant("kb");
    let svc = Service::start(&s);
    let r = svc.call(&s, "soup_dump", json!({"cauldron": "kb", "text": "through the door"}));
    assert_eq!(r["structuredContent"]["dump_id"], "DM-3", "{r}");
    let g = svc.call(&s, "soup_dump_get", json!({"cauldron": "kb", "id": "DM-1"}));
    assert_eq!((g["structuredContent"]["dump"]["state"].clone(), g["structuredContent"]["erasure"].clone()), (json!("removed"), json!("finished")), "{g}");
    svc.stop();
    let left = files_holding(&dir, marker);
    assert!(left.is_empty(), "left behind: {left:?}");

    // Without the seam in the build, a set variable is refused; here it is
    // in the build, so an unknown step is refused before anything changes.
    let s = two_dumps("d7-unknown", "PLANTED-UNKNOWN-STEP");
    let o = remove_stopped_after(&s, "no-such-step");
    assert_eq!(o.rc, 1, "{}", o.err);
    assert!(o.err.contains("names no step"), "{}", o.err);
    assert_eq!(dump_state(&s, "DM-1")["dump"]["state"], "received");
}

/// The bridge answers a JSON-RPC batch itself (`-32600`, nothing relayed),
/// and a `session_id` left as an unsubstituted `${...}` names this session.
#[test]
fn d6_a_batch_is_answered_by_the_bridge_and_a_placeholder_session_id_is_this_session() {
    let s = Scratch::new("d6", &["kb"]);
    s.grant("kb");
    let config = s.0.join("claude-config");
    let dir = config.join("projects").join("-work-c");
    std::fs::create_dir_all(&dir).unwrap();
    let tc = transcript(3, "/work/c", "gamma");
    std::fs::write(dir.join("sess-c.jsonl"), &tc).unwrap();
    let cfg = config.display().to_string();
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let mut b = Bridge::start(&s, &s.project(), &[], &[("CLAUDE_CONFIG_DIR", cfg.as_str()), ("CLAUDE_CODE_SESSION_ID", "sess-c")], &pids);
    init(&mut b);
    {
        let w = b.stdin.as_mut().unwrap();
        writeln!(w, "{}", json!([{"jsonrpc": "2.0", "id": 7, "method": "tools/list"}])).unwrap();
        w.flush().unwrap();
        let mut line = String::new();
        b.stdout.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!((v["error"]["code"].clone(), v["id"].clone()), (json!(-32600), Value::Null), "{v}");
    }
    let r = b.call(8, "soup_dump_session", json!({"session_id": "${CLAUDE_SESSION_ID}"}));
    assert_eq!(r["isError"], json!(false), "{r}");
    assert_eq!((r["structuredContent"]["session_id"].clone(), r["structuredContent"]["sha256"].clone()), (json!("sess-c"), json!(sha256_hex(&tc))));
    let (rc, _, err) = b.finish();
    assert_eq!(rc, 0, "{err}");
    svc.stop();
}

// ------------------------------------------------------------- the review

impl Scratch {
    /// The verb on a pseudo-terminal (`script -q /dev/null cauldron <args>`,
    /// the in-test way to give a command a terminal), with `input` typed in
    /// line by line, each once the terminal shows `prompt_marker` again (or
    /// at once when there is none). Everything it prints arrives on stdout.
    fn on_tty(&self, args: &[&str], pids: &[u32], input: &[&str], prompt_marker: Option<&str>) -> Out {
        use std::io::Read;
        let mut full = vec!["-q", "/dev/null", CAULDRON];
        full.extend_from_slice(args);
        let cmd = self.command(&[], pids);
        let mut child = Command::new("/usr/bin/script");
        child.args(&full).current_dir(self.0.join("work"));
        for (k, v) in cmd.get_envs() {
            match v {
                Some(v) => child.env(k, v),
                None => child.env_remove(k),
            };
        }
        let mut child = child.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let reader = {
            let (out, mut stdout) = (out.clone(), child.stdout.take().unwrap());
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = stdout.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    out.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            })
        };
        let mut stdin = child.stdin.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        for (i, line) in input.iter().enumerate() {
            if let Some(m) = prompt_marker {
                loop {
                    let seen = String::from_utf8_lossy(&out.lock().unwrap()).matches(m).count();
                    if seen > i || child.try_wait().unwrap().is_some() || Instant::now() > deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            let _ = stdin.write_all(format!("{line}\n").as_bytes());
        }
        let status = child.wait().unwrap();
        drop(stdin);
        reader.join().unwrap();
        let text = String::from_utf8_lossy(&out.lock().unwrap()).replace("\r\n", "\n");
        Out { rc: status.code().unwrap_or(-1), out: text, err: String::new() }
    }

    /// `ripples review` (JSON).
    fn queue(&self, extra: &[&str]) -> Value {
        let mut a = vec!["ripples", "review", "--env", "dev", "--cauldron", "kb"];
        a.extend_from_slice(extra);
        let o = self.run(&a, &[]);
        assert_eq!(o.rc, 0, "{}", o.err);
        o.json()
    }
}

/// The JSON object a terminal run printed (from its first `{` line to the
/// matching closing line).
fn printed_json(text: &str) -> Value {
    let start = text.find("{\n").unwrap_or_else(|| panic!("no JSON in: {text}"));
    let end = text[start..].find("\n}").map(|e| start + e + 2).unwrap_or_else(|| panic!("no JSON end in: {text}"));
    serde_json::from_str(&text[start..end]).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// **`ripples approve | reject`, at a terminal, through the real service.**
/// Without a terminal each is refused with the exact command and writes
/// nothing; at one, a dump is rejected with a reason, a run approved, a
/// rejected record refused approval until it is unrejected, all through the
/// running `mcp` service; the queue and the reads follow; with the service
/// stopped the command says how to start it.
#[test]
fn rv1_ripples_approve_and_reject_at_a_terminal() {
    let s = Scratch::new("rv1", &["kb"]);
    let records = s.write("example.json", &json!(example()));
    assert_eq!(s.import("kb", &records, &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y")]);
    let b = crate::ripples::ripple_write(&s.soup("kb"), "run-b", "DM-2", "sonnet", &[("task", "three", "z"), ("idea", "four", "w")]);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    assert_eq!(s.queue(&[])["counts"]["unreviewed"], 4);
    // No terminal: refused before anything is sent, with the command.
    let o = s.run(&["ripples", "reject", "--dump", "DM-1", "--env", "dev", "--cauldron", "kb", "--reason", "not mine"], &pids);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("REFUSED: rejecting a ripple's records is the owner's act at a terminal") && o.err.contains("unless the owner has let agents") && o.err.contains("cauldron owner-acts open"), "{}", o.err);
    assert!(o.err.contains("cauldron ripples reject --dump DM-1 --env dev --cauldron kb --reason 'not mine'"), "{}", o.err);
    let o = s.run(&["ripples", "approve", "--run", "run-b", "--env", "dev", "--cauldron", "kb"], &pids);
    assert!(o.err.contains("cannot approve records by itself") && o.err.contains("cauldron ripples approve --run run-b --env dev --cauldron kb"), "{}", o.err);
    assert_eq!(s.queue(&[])["counts"]["unreviewed"], 4, "nothing was written");
    // Usage, before the terminal.
    for (args, says) in [
        (vec!["ripples", "approve", "--dump", "DM-1", "--run", "run-a", "--env", "dev", "--cauldron", "kb"], "one way"),
        (vec!["ripples", "approve", "--env", "dev", "--cauldron", "kb"], "one way"),
        (vec!["ripples", "approve", "--dump", "DM-1", "--env", "dev", "--cauldron", "kb", "--reason", "x"], "unknown option"),
        (vec!["ripples", "reject", "--dump", "DM-1", "--env", "dev"], "only as flags"),
        (vec!["ripples", "reject", "--dump", "DM-1", "--env", "dev", "--cauldron", "kb", "--unreject", "--reason", "x"], "--unreject"),
    ] {
        let o = s.run(&args, &pids);
        assert_ne!(o.rc, 0, "{args:?}");
        assert!(o.err.contains(says), "{args:?}: {}", o.err);
    }
    // At a terminal.
    let o = s.on_tty(&["ripples", "reject", "--dump", "DM-1", "--env", "dev", "--cauldron", "kb", "--reason", "not mine"], &pids, &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let v = printed_json(&o.out);
    assert_eq!((v["outcome"].clone(), v["rejected"].as_array().unwrap().len()), (json!("rejected"), 2), "{v}");
    assert!(o.out.contains("reject: 2 record(s) written, 0 skipped"), "{}", o.out);
    let o = s.on_tty(&["ripples", "approve", "--run", "run-b", "--env", "dev", "--cauldron", "kb"], &pids, &[], None);
    assert_eq!(printed_json(&o.out)["approved"].as_array().unwrap().len(), 2, "{}", o.out);
    let q = s.queue(&[]);
    assert_eq!((q["counts"]["unreviewed"].clone(), q["counts"]["approved"].clone(), q["counts"]["rejected"].clone()), (json!(0), json!(2), json!(2)), "{q}");
    // Rejected: kept, out of the reads unless asked, not approvable.
    let o = s.run(&["soup", "nodes", "--env", "dev", "--cauldron", "kb", "--limit", "100"], &pids);
    let n = o.json();
    assert_eq!(n["rejected_hidden"], json!(2), "{n}");
    let o = s.run(&["soup", "nodes", "--env", "dev", "--cauldron", "kb", "--limit", "100", "--include-rejected"], &pids);
    assert!(o.json()["nodes"].as_array().unwrap().iter().any(|x| x["id"] == a[0].as_str() && x["rejected"] == json!(true)));
    let o = s.run(&["soup", "get", &a[0], "--env", "dev", "--cauldron", "kb"], &pids);
    let rec = &o.json()["records"][0];
    assert_eq!((rec["rejected"].clone(), rec["meta"]["rejected"]["reason"].clone()), (json!(true), json!("not mine")), "{rec}");
    let o = s.on_tty(&["ripples", "approve", &a[0], "--env", "dev", "--cauldron", "kb"], &pids, &[], None);
    let v = printed_json(&o.out);
    assert_eq!(v["outcome"], json!("nothing-to-approve"), "{v}");
    let o = s.on_tty(&["soup", "approve", &a[0], "--env", "dev", "--cauldron", "kb"], &pids, &[], None);
    assert_eq!(printed_json(&o.out)["outcome"], json!("nothing-to-approve"), "`soup approve` too: {}", o.out);
    // Unrejected: back in the queue; then approvable.
    let o = s.on_tty(&["ripples", "reject", "--dump", "DM-1", "--env", "dev", "--cauldron", "kb", "--unreject"], &pids, &[], None);
    assert_eq!(printed_json(&o.out)["unrejected"].as_array().unwrap().len(), 2, "{}", o.out);
    assert_eq!(s.queue(&["--dump", "DM-1"])["counts"]["unreviewed"], 2);
    let o = s.on_tty(&["ripples", "approve", &a[0], &a[1], "--env", "dev", "--cauldron", "kb"], &pids, &[], None);
    assert_eq!(printed_json(&o.out)["approved"].as_array().unwrap().len(), 2, "{}", o.out);
    let o = s.on_tty(&["ripples", "approve", "--dump", "DM-2", "--env", "dev", "--cauldron", "kb", "--unapprove"], &pids, &[], None);
    let mut un: Vec<String> = printed_json(&o.out)["unapproved"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    un.sort();
    let mut bs = b.clone();
    bs.sort();
    assert_eq!(un, bs, "{}", o.out);
    assert_eq!(s.queue(&[])["counts"]["unreviewed"], 2);
    let audit = s.calls();
    for t in ["soup_reject", "soup_approve", "soup_unreject", "soup_unapprove"] {
        assert!(audit.iter().any(|l| l["tool"] == t && l["client"] == "cli"), "{t}");
    }
    // The service stopped: the command says how to start it.
    svc.stop();
    let o = s.on_tty(&["ripples", "approve", "--dump", "DM-2", "--env", "dev", "--cauldron", "kb"], &[], &[], None);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.out.contains("the `mcp` service writes the decision") && o.out.contains("start mcp"), "{}", o.out);
}

/// **`ripples review --interactive`**, at a terminal, through the real
/// service: groups come newest first; `s` skips one (nothing written), a
/// bad key re-asks, numbers mark records and `w` writes the marks (with a
/// reason for the rejected), `r` rejects a whole group, `q` quits; record
/// text is shown on one line; without a terminal it is refused with the
/// command.
#[test]
fn rv2_the_interactive_pass() {
    let s = Scratch::new("rv2", &["kb"]);
    let records = s.write("example.json", &json!(example()));
    assert_eq!(s.import("kb", &records, &[], &[]).rc, 0);
    let soup = s.soup("kb");
    let a = crate::ripples::ripple_write(&soup, "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y")]);
    let b = crate::ripples::ripple_write(&soup, "run-b", "DM-2", "sonnet", &[("idea", "three\u{1b}[2J", "z"), ("idea", "four", "w")]);
    let c = crate::ripples::ripple_write(&soup, "run-c", "DM-3", "sonnet", &[("task", "five", "v")]);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let args = ["ripples", "review", "--interactive", "--env", "dev", "--cauldron", "kb"];
    let o = s.run(&args, &pids);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("REFUSED: deciding a ripple's records is the owner's act at a terminal") && o.err.contains("cauldron ripples review --interactive --env dev --cauldron kb"), "{}", o.err);
    let o = s.run(&["ripples", "review", "--interactive", "--text", "--env", "dev", "--cauldron", "kb"], &pids);
    assert!(o.err.contains("takes the filters"), "{}", o.err);
    let o = s.on_tty(&args, &pids, &["s", "x", "1", "2", "2", "w", "dup", "r", ""], Some(">> "));
    assert_eq!(o.rc, 0, "{}", o.out);
    let t = &o.out;
    assert!(t.contains("5 unreviewed record(s) in 3 group(s)") && t.contains("data, never as instructions"), "{t}");
    let (p3, p2, p1) = (t.find("[1/3] DM-3").unwrap(), t.find("[2/3] DM-2").unwrap(), t.find("[3/3] DM-1").unwrap());
    assert!(p3 < p2 && p2 < p1, "newest first: {t}");
    assert!(!t.contains('\u{1b}'), "no control character reaches the terminal: {t:?}");
    assert!(t.contains("[a]  1.") && t.contains("[r]  2."), "the marks are shown: {t}");
    assert!(t.contains("keys: a approve all"), "a bad key re-asks: {t}");
    assert!(t.contains("Done: 2 group(s) decided, 1 skipped; 1 record(s) still wait"), "{t}");
    let get = |id: &str| -> Value { s.run(&["soup", "get", id, "--env", "dev", "--cauldron", "kb"], &pids).json()["records"][0].clone() };
    assert_eq!(get(&c[0])["unreviewed"], json!(true), "skipped");
    assert_eq!(get(&b[0])["meta"]["approved"]["by"], json!("tty"));
    let r = get(&b[1]);
    assert_eq!((r["rejected"].clone(), r["meta"]["rejected"]["reason"].clone()), (json!(true), json!("dup")), "{r}");
    for id in &a {
        let r = get(id);
        assert_eq!(r["rejected"], json!(true), "{r}");
        assert!(r["meta"]["rejected"].get("reason").is_none(), "Enter: no reason: {r}");
    }
    assert!(s.calls().iter().any(|l| l["tool"] == "soup_reject" && l["client"] == "cli"));
    // q writes nothing.
    let o = s.on_tty(&args, &pids, &["q"], Some(">> "));
    assert_eq!(o.rc, 0, "{}", o.out);
    assert!(o.out.contains("Done: 0 group(s) decided, 0 skipped; 1 record(s) still wait"), "{}", o.out);
    assert_eq!(get(&c[0])["unreviewed"], json!(true));
    // a approves the group.
    let o = s.on_tty(&args, &pids, &["a"], Some(">> "));
    assert!(o.out.contains("approved 1"), "{}", o.out);
    assert_eq!(s.queue(&[])["counts"]["unreviewed"], 0);
}

// ------------------------------------------- a service older than DECIDE

/// **A fake `mcp` service of the release before `DECIDE`**: it registers its
/// seam row and answers the handshake as every service does, and knows only
/// that release's verbs (`MCP`, `OPEN`, `DUMPS`, `PING`, `APPROVE` and the
/// ripple verbs); any other word gets the seam's `ERR unknown request`. Its
/// `APPROVE` answers as that release's did, which read only `ids` and
/// `unapprove` and approved: every line it is sent is kept, so a test can
/// show what a newer command line sent it. Its door answers `initialize`
/// with `version`, as the real door does.
struct OldMcp {
    se: env::seams::ServiceEnv,
    addr: SocketAddr,
    door: SocketAddr,
    heard: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl env::warnings::ServesWarnings for OldMcp {
    const SHAPE: &'static str = env::warnings::SHAPE;
    fn warnings(&self, _asked: &str) -> anyhow::Result<env::warnings::Export> {
        anyhow::bail!("this fake serves no warnings")
    }
}

impl OldMcp {
    fn says(&self, c: &mut env::warnings::Conn<'_>, line: &str) -> anyhow::Result<()> {
        c.line(line)?;
        Ok(())
    }
    fn v_mcp(&self, c: &mut env::warnings::Conn<'_>, _r: &str) -> anyhow::Result<()> {
        self.says(c, &format!("http://{}/mcp", self.door))
    }
    fn v_empty(&self, c: &mut env::warnings::Conn<'_>, _r: &str) -> anyhow::Result<()> {
        self.says(c, "[]")
    }
    fn v_ping(&self, c: &mut env::warnings::Conn<'_>, _r: &str) -> anyhow::Result<()> {
        self.says(c, "PONG")
    }
    fn v_ripple(&self, c: &mut env::warnings::Conn<'_>, _r: &str) -> anyhow::Result<()> {
        self.says(c, r#"{"ok":false,"code":"unauthorized","message":"no"}"#)
    }
    /// That release's `APPROVE`: `{cauldron, ids, unapprove?}`, nothing else read.
    fn v_approve(&self, c: &mut env::warnings::Conn<'_>, rest: &str) -> anyhow::Result<()> {
        self.heard.lock().unwrap().push(format!("APPROVE {rest}"));
        let body: Value = rest.split_once(' ').and_then(|(_, b)| serde_json::from_str(b).ok()).unwrap_or(Value::Null);
        let un = body["unapprove"].as_bool().unwrap_or(false);
        let answer = match body["ids"].as_array() {
            Some(ids) if !ids.is_empty() => {
                let key = if un { "unapproved" } else { "approved" };
                json!({"ok": true, key: ids, "skipped": [], "outcome": key})
            }
            _ => json!({"ok": false, "code": "usage", "message": "`ids` must be a list of 1 to 500 record ids. Nothing was written."}),
        };
        self.says(c, &answer.to_string())
    }
}

impl env::warnings::Service for OldMcp {
    const VERBS: &'static [env::warnings::Verb<OldMcp>] = &[
        env::warnings::Verb::new("MCP", OldMcp::v_mcp),
        env::warnings::Verb::new("OPEN", OldMcp::v_empty),
        env::warnings::Verb::new("DUMPS", OldMcp::v_empty),
        env::warnings::Verb::new("PING", OldMcp::v_ping),
        env::warnings::Verb::new("RDUMP", OldMcp::v_ripple),
        env::warnings::Verb::new("RTEXT", OldMcp::v_ripple),
        env::warnings::Verb::new("RREAD", OldMcp::v_ripple),
        env::warnings::Verb::new("RRUN", OldMcp::v_ripple),
        env::warnings::Verb::new("RPUT", OldMcp::v_ripple),
        env::warnings::Verb::new("RCHECK", OldMcp::v_ripple),
        env::warnings::Verb::new("APPROVE", OldMcp::v_approve),
        env::warnings::Verb::new("RDUMPS", OldMcp::v_ripple),
        env::warnings::Verb::new("RNOTE", OldMcp::v_ripple),
        env::warnings::Verb::new("RFAIL", OldMcp::v_ripple),
    ];
    fn service_env(&self) -> &env::seams::ServiceEnv {
        &self.se
    }
    fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Start [`OldMcp`] in this process, answering `version` from its door;
/// it registers itself in the scratch environment's seam table. Answers
/// what it heard (shared) once it is up.
fn old_mcp(s: &Scratch, version: &str) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    use std::net::TcpListener;
    // The files the real service mints at its start.
    for f in ["door.secret", "approve.secret"] {
        std::fs::write(s.mcp_dir().join(f), format!("fake-{f}-value\n")).unwrap();
    }
    let door = TcpListener::bind("127.0.0.1:0").unwrap();
    let door_addr = door.local_addr().unwrap();
    let v = version.to_string();
    std::thread::spawn(move || {
        for c in door.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                if let Some(n) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = n.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            let _ = r.read_exact(&mut body);
            let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let answer = json!({"jsonrpc": "2.0", "id": req["id"], "result": {"serverInfo": {"name": "cauldron-mcp", "version": v}}}).to_string();
            let mut c = c;
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len());
        }
    });
    let seam = TcpListener::bind("127.0.0.1:0").unwrap();
    let se = env::seams::ServiceEnv {
        service: "mcp".into(),
        env: "dev".into(),
        data_root: s.data(),
        data_dir: s.mcp_dir(),
        seams: s.data().join("seams.json"),
        boot_token: format!("old-boot-{}", std::process::id()),
        version: version.into(),
        commit: String::new(),
        dirty: false,
        bind: "127.0.0.1:0".into(),
    };
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let svc = std::sync::Arc::new(OldMcp { se, addr: seam.local_addr().unwrap(), door: door_addr, heard: heard.clone() });
    let attested = env::attest_warnings!(OldMcp);
    svc.se.register(&svc.addr, "old-cauldron-mcp", "seam", &attested).unwrap();
    std::thread::spawn(move || {
        for c in seam.incoming().flatten() {
            let svc = svc.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut w = c;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let line = line.trim().to_string();
                    let (verb, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                    let mut conn = env::warnings::Conn { out: &mut w, frames: 0 };
                    match env::warnings::dispatch(&*svc, verb, rest.trim(), &mut conn) {
                        Ok(env::warnings::Answered::Yes) => {}
                        Ok(env::warnings::Answered::NotMine) => {
                            svc.heard.lock().unwrap().push(line.clone());
                            let _ = conn.line(&format!("ERR unknown request `{verb}`"));
                        }
                        Err(_) => return,
                    }
                    let _ = w.flush();
                }
            });
        }
    });
    heard
}

/// **A newer command line never decides through an older `mcp` service.**
/// Against a fake service of the release before `DECIDE` (the same version
/// number, as between two builds of one version): `ripples reject`,
/// `--unreject`, `ripples approve`, `soup approve` and the interactive pass
/// are each refused, naming the upgrade, and the old service is sent no
/// `APPROVE` (the verb it would read as an approval). Against one that
/// says it is an older version: refused before anything but the version
/// question is asked.
#[test]
fn rv3_an_older_mcp_service_is_never_sent_a_decision_it_would_misread() {
    let s = Scratch::new("rv3", &["kb"]);
    let records = s.write("example.json", &json!(example()));
    assert_eq!(s.import("kb", &records, &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y")]);
    s.grant("kb");
    let heard = old_mcp(&s, env!("CARGO_PKG_VERSION"));
    let me = [std::process::id()];
    let before = s.queue(&[]);
    for args in [
        vec!["ripples", "reject", a[0].as_str(), "--env", "dev", "--cauldron", "kb", "--reason", "spam"],
        vec!["ripples", "reject", a[1].as_str(), "--env", "dev", "--cauldron", "kb", "--unreject"],
        vec!["ripples", "reject", "--dump", "DM-1", "--env", "dev", "--cauldron", "kb"],
        vec!["ripples", "approve", "--run", "run-a", "--env", "dev", "--cauldron", "kb"],
        vec!["soup", "approve", a[0].as_str(), "--env", "dev", "--cauldron", "kb"],
    ] {
        let o = s.on_tty(&args, &me, &[], None);
        assert_ne!(o.rc, 0, "{args:?}: {}", o.out);
        assert!(o.out.contains("REFUSED: the running `mcp` service does not know `DECIDE`") && o.out.contains("It wrote nothing, and nothing was decided"), "{args:?}: {}", o.out);
        assert!(o.out.contains("the `mcp` service must be upgraded with the command line") && o.out.contains("--replace-running") && o.out.contains("build mcp --env-copy"), "{args:?}: {}", o.out);
    }
    let o = s.on_tty(&["ripples", "review", "--interactive", "--env", "dev", "--cauldron", "kb"], &me, &["r", "spam"], Some(">> "));
    assert!(o.out.contains("does not know `DECIDE`"), "{}", o.out);
    let lines = heard.lock().unwrap().clone();
    assert!(!lines.is_empty() && lines.iter().all(|l| l.starts_with("DECIDE ")), "only the verb it does not know was sent: {lines:?}");
    assert_eq!(s.queue(&[])["counts"], before["counts"], "nothing was written");
    assert!(s.calls().is_empty(), "nothing was audited as a decision");

    // An older version: nothing is sent but the version question.
    let s2 = Scratch::new("rv3b", &["kb"]);
    assert_eq!(s2.import("kb", &s2.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let b = crate::ripples::ripple_write(&s2.soup("kb"), "run-b", "DM-1", "sonnet", &[("idea", "one", "x")]);
    s2.grant("kb");
    let heard2 = old_mcp(&s2, "0.1.0");
    let o = s2.on_tty(&["ripples", "reject", b[0].as_str(), "--env", "dev", "--cauldron", "kb"], &me, &[], None);
    assert_ne!(o.rc, 0, "{}", o.out);
    assert!(o.out.contains("REFUSED: the running `mcp` service is cauldron 0.1.0, older than this cauldron") && o.out.contains("Nothing was sent and nothing was written") && o.out.contains("Version skew:"), "{}", o.out);
    assert!(heard2.lock().unwrap().is_empty(), "nothing was sent: {:?}", heard2.lock().unwrap());
    assert_eq!(s2.queue(&[])["counts"]["unreviewed"], 1);
}

/// **The same, against a real older binary**, when one is named in
/// `CAULDRON_OLD_MCP` (it is not built here): reject changes nothing and
/// says why. Which refusal is right depends on the binary, so it is asked
/// first (its `serverInfo.version`, and whether its seam knows `DECIDE`):
///
/// - a version older than this build's: the version-skew check refuses
///   before anything is sent ("Nothing was sent and nothing was written");
/// - this build's version, without `DECIDE` (a build from before it): it is
///   sent `DECIDE`, answers `ERR unknown request`, and nothing is decided;
/// - this build's version WITH `DECIDE` (a build of this very release, such
///   as the published one before the version is bumped): it is not older,
///   and it decides as this build does; rv4 then checks only that the one
///   reject lands as the owner's, and says it checked nothing older.
///
/// (Run with 0.1.16 at 0.1.17 it took the first branch; with 0.1.17 at
/// 0.1.17 it used to expect a refusal from a service that is not older and
/// failed: the input broke the test's premise, not the code.)
#[test]
fn rv4_the_real_older_mcp_service_rejects_nothing_and_says_why() {
    let Some(old) = std::env::var_os("CAULDRON_OLD_MCP") else {
        eprintln!("rv4: CAULDRON_OLD_MCP names no binary; the fake of rv3 stands in");
        return;
    };
    let s = Scratch::new("rv4", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y")]);
    s.grant("kb");
    let svc = Service::start_bin(&s, Path::new(&old));
    let pids = [svc.pid()];
    let version = svc.server_version(&s);
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let older = parts(&version) < parts(env!("CARGO_PKG_VERSION"));
    let knows_decide = !s.seam_line("DECIDE").starts_with("ERR unknown request");
    if !older && knows_decide {
        eprintln!(
            "rv4: CAULDRON_OLD_MCP is cauldron {version}, this build's version, and knows DECIDE: it is not an older service, so \
             nothing older is checked; only that it decides as this build does"
        );
        let o = s.on_tty(&["ripples", "reject", a[0].as_str(), "--env", "dev", "--cauldron", "kb", "--reason", "spam"], &pids, &[], None);
        assert_eq!(o.rc, 0, "{}", o.out);
        assert!(o.out.contains("\"outcome\": \"rejected\""), "{}", o.out);
        assert_eq!(s.queue(&[])["counts"]["rejected"], json!(1));
        svc.stop();
        return;
    }
    for args in [
        vec!["ripples", "reject", a[0].as_str(), "--env", "dev", "--cauldron", "kb", "--reason", "spam"],
        vec!["ripples", "reject", a[1].as_str(), "--env", "dev", "--cauldron", "kb", "--unreject"],
        vec!["soup", "approve", a[0].as_str(), "--env", "dev", "--cauldron", "kb"],
    ] {
        let o = s.on_tty(&args, &pids, &[], None);
        println!("rv4 {}:\n{}", args.join(" "), o.out);
        assert_ne!(o.rc, 0, "{args:?}: {}", o.out);
        assert!(o.out.contains("REFUSED:") && o.out.contains("must be upgraded"), "{args:?}: {}", o.out);
        if older {
            assert!(o.out.contains(&format!("is cauldron {version}, older than this cauldron")) && o.out.contains("Nothing was sent and nothing was written"), "{args:?}: {}", o.out);
        } else {
            assert!(o.out.contains("does not know `DECIDE`") && o.out.contains("nothing was decided"), "{args:?}: {}", o.out);
        }
    }
    let q = s.queue(&[]);
    assert_eq!((q["counts"]["unreviewed"].clone(), q["counts"]["approved"].clone(), q["counts"]["rejected"].clone()), (json!(2), json!(0), json!(0)), "{q}");
    assert!(!s.calls().iter().any(|l| l["tool"] == "soup_approve"), "{:?}", s.calls());
    svc.stop();
}

impl Service {
    /// The version the service says it is (`initialize`, `serverInfo.version`).
    fn server_version(&self, s: &Scratch) -> String {
        let body = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}).to_string();
        let mut c = TcpStream::connect(self.door).unwrap();
        write!(
            c,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.door.port(),
            s.secret(),
            body.len()
        )
        .unwrap();
        let mut raw = String::new();
        c.read_to_string(&mut raw).unwrap();
        let v: Value = serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap();
        v["result"]["serverInfo"]["version"].as_str().unwrap_or("").to_string()
    }
}

/// **The version the older-service tests were last checked against.**
/// Which refusal an older `mcp` service draws depends on how its version
/// compares with this build's: one of the SAME version that does not know
/// `DECIDE` is sent it and answers `ERR unknown request` ("nothing was
/// decided"); one of an OLDER version is refused by the version-skew check
/// before anything is sent ("nothing was written"). rv4 runs against a real
/// older binary only when `CAULDRON_OLD_MCP` names one, so when the
/// workspace version moved past that binary's, rv4 changed branch and broke
/// while the suite (run without the variable) stayed green. This test fails
/// whenever the workspace version is not this constant, so every release
/// bump makes someone check rv3 and rv4 against the new relation and move
/// it.
const OLDER_SERVICE_TESTS_CHECKED_AT: &str = "0.1.19";

#[test]
fn rv0_the_older_service_tests_were_checked_against_the_workspace_version() {
    assert_eq!(
        env!("CARGO_PKG_VERSION"),
        OLDER_SERVICE_TESTS_CHECKED_AT,
        "the workspace version moved: check rv3 and rv4 (which refusal an older `mcp` service draws now: the version-skew check, or `DECIDE` unknown), \
         run rv4 with CAULDRON_OLD_MCP naming the previous release's binary, then set OLDER_SERVICE_TESTS_CHECKED_AT to the new version"
    );
}

/// **No record write sets or clears a review flag**: `soup import` direct
/// (and `--dry-run`), `soup import` while the service holds the soup (it
/// would be forwarded), `soup_put`, `soup_link` and the ripple's own batch
/// (`services/mcp` `ripple` tests) each refuse a record carrying
/// `meta.approved` or `meta.rejected` in any casing or nesting, and write
/// nothing; only the owner's decision does.
#[test]
fn w3_no_record_write_sets_a_review_flag() {
    let s = Scratch::new("w3", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let rec = |meta_key: &str, meta: Value| -> Value {
        let mut r = task("t", "a record that reviews itself");
        r[meta_key] = meta;
        r
    };
    let variants = [
        ("meta", json!({"rejected": {"at": "2026-01-01T00:00:00Z", "by": "owner"}})),
        ("meta", json!({"approved": {"at": "2026-01-01T00:00:00Z", "by": "owner"}})),
        ("meta", json!({"rejected": null})),
        ("meta", json!({"Approved": true})),
        ("meta", json!({"REJECTED": true})),
        ("meta", json!({"meta": {"rejected": true}})),
        ("Meta", json!({"rejected": true})),
        ("meta.rejected", json!({"by": "owner"})),
        ("rejected", json!({"by": "owner"})),
    ];
    let before = s.node_count("kb");
    for (k, m) in &variants {
        let file = s.write("flag.json", &json!([rec(k, m.clone())]));
        for extra in [&["--remint"][..], &["--remint", "--dry-run"][..]] {
            let o = s.import("kb", &file, extra, &[]);
            assert_ne!(o.rc, 0, "{k} {m} {extra:?}: {}", o.out);
            assert!(o.err.contains("the review flags (`meta.approved`, `meta.rejected`) are the owner's"), "{k} {m}: {}", o.err);
        }
    }
    assert_eq!(s.node_count("kb"), before, "nothing landed directly");
    // The service holds the soup: the batch would be forwarded; it is not.
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    let before = s.node_count("kb");
    let puts = s.calls().iter().filter(|l| l["tool"] == "soup_put").count();
    for (k, m) in &variants {
        let o = s.import("kb", &s.write("flag.json", &json!([rec(k, m.clone())])), &["--remint"], &pids);
        assert_ne!(o.rc, 0, "{k} {m}: {}", o.out);
        assert!(o.err.contains("are the owner's"), "{k} {m}: {}", o.err);
        // And straight to the service, as a session or an older command line would send it.
        let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [rec(k, m.clone())]}));
        assert_eq!(r["isError"], json!(true), "{k} {m}: {r}");
        let refused = &r["structuredContent"]["refused"];
        assert_eq!(refused["code"], json!("usage"), "{k} {m}: {r}");
        assert!(refused["message"].as_str().unwrap().contains("are the owner's"), "{k} {m}: {r}");
    }
    assert_eq!(s.node_count("kb"), before, "nothing landed through the service");
    assert_eq!(s.calls().iter().filter(|l| l["tool"] == "soup_put").count(), puts + variants.len(), "each refusal audited, none forwarded");
    // A link carries edges only: a review flag has nowhere to go.
    let r = svc.call(&s, "soup_link", json!({"cauldron": "kb", "links": [{"from": "R-1", "kind": "ref", "to": "R-2", "meta": {"rejected": true}}]}));
    assert_eq!(r["isError"], json!(true), "{r}");
    svc.stop();
}

/// **No record write sets `meta.ripple`** (issue #23, item 2.10): `soup
/// import` direct (with and without `--remint`, and `--dry-run`), `soup
/// import` while the service holds the soup (it would be forwarded) and
/// `soup_put` each refuse a record carrying ripple provenance in any
/// casing or nesting, as `ripple_provenance`, and write nothing; only the
/// door, on a ripple's batch, stamps it.
#[test]
fn w3b_no_record_write_sets_ripple_provenance() {
    let s = Scratch::new("w3b", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let fake = json!({"package": "dump", "version": "v1", "run": "made-up", "model": "opus"});
    let rec = |key: &str, v: Value| -> Value {
        let mut r = task("t", "a record that claims a ripple wrote it");
        r[key] = v;
        r
    };
    let variants = [
        ("meta", json!({"ripple": fake})),
        ("meta", json!({"Ripple": fake})),
        ("meta", json!({"ripple": null})),
        ("meta", json!({"meta": {"ripple": fake}})),
        ("Meta", json!({"ripple": fake})),
        ("meta.ripple", fake.clone()),
        ("ripple", fake.clone()),
    ];
    let before = s.node_count("kb");
    for (k, m) in &variants {
        let file = s.write("prov.json", &json!([rec(k, m.clone())]));
        for extra in [&[][..], &["--dry-run"][..], &["--remint"][..], &["--remint", "--dry-run"][..]] {
            let o = s.import("kb", &file, extra, &[]);
            assert_ne!(o.rc, 0, "{k} {m} {extra:?}: {}", o.out);
            assert!(o.err.contains("refused: ripple_provenance"), "{k} {m} {extra:?}: {}", o.err);
        }
    }
    assert_eq!(s.node_count("kb"), before, "nothing landed directly");
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    let before = s.node_count("kb");
    for (k, m) in &variants {
        let o = s.import("kb", &s.write("prov.json", &json!([rec(k, m.clone())])), &["--remint"], &pids);
        assert_ne!(o.rc, 0, "{k} {m}: {}", o.out);
        assert!(o.err.contains("refused: ripple_provenance"), "{k} {m}: {}", o.err);
        let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [rec(k, m.clone())]}));
        let refused = &r["structuredContent"]["refused"];
        assert_eq!((refused["code"].clone(), refused["detail"]["refused"].clone()), (json!("usage"), json!("ripple_provenance")), "{k} {m}: {r}");
    }
    assert_eq!(s.node_count("kb"), before, "nothing landed through the service");
    svc.stop();
}

/// **`soup import` writes back what a read shows** (issue #23, item 2.4):
/// the keys a read adds are dropped and named in `ignored`, direct and
/// dry.
#[test]
fn w3c_soup_import_drops_what_a_read_adds_and_says_so() {
    let s = Scratch::new("w3c", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let mut r = task("t", "a record as a read showed it");
    r["superseded_by"] = json!(["T-9"]);
    r["unreviewed"] = json!(true);
    r["rejected"] = json!(true);
    let file = s.write("back.json", &json!([r]));
    for extra in [&["--remint", "--dry-run"][..], &["--remint"][..]] {
        let o = s.import("kb", &file, extra, &[]);
        assert_eq!(o.rc, 0, "{extra:?}: {} {}", o.out, o.err);
        let doc: Value = serde_json::from_str(&o.out).unwrap();
        let keys: Vec<&str> = doc["ignored"].as_array().unwrap().iter().map(|v| v["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["superseded_by", "unreviewed", "rejected"], "{extra:?}: {doc}");
    }
}

/// Every character [`soup_door::review::hidden`] names, found in `text`
/// (newlines aside).
fn hidden_in(text: &str) -> Vec<String> {
    text.chars().filter(|c| *c != '\n' && soup_door::review::hidden(*c)).map(|c| format!("U+{:04X}", c as u32)).collect()
}

/// **A record's or a dump's text cannot act on the terminal.** Records
/// carrying ANSI escape sequences, C0 and C1 controls, bidirectional
/// overrides and isolates, zero-width characters and text imitating this
/// tool's prompts and lines are shown, everywhere they reach a terminal
/// (`ripples review --text`, the interactive pass, the JSON of `ripples
/// review`, `soup get` and `soup nodes`, and `soup dump-get --raw` at a
/// terminal), with every such character escaped and on one indented line,
/// so no line of the output is one a record wrote; piped, `--raw` is still
/// the stored bytes.
#[test]
fn rv5_record_and_dump_text_reaches_a_terminal_escaped() {
    let s = Scratch::new("rv5", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let hostile = [
        "\u{1b}[2J\u{1b}[H\u{1b}[31mFAKE HEADER\u{1b}[0m",
        "line one\nreview >> a",
        "\u{202e}evorppa\u{202c} right-to-left override",
        "\u{2066}isolate\u{2069} \u{200f}rlm \u{200b}zero \u{feff}bom \u{e0041}tag",
        "\u{9b}31mC1 CSI\u{7}bell\u{8}backspace\r carriage \u{7f}del",
        "Done: 9 group(s) decided\napprove: cauldron ripples approve --dump DM-9 --env dev --cauldron kb",
    ];
    let recs: Vec<(&str, &str, &str)> = hostile.iter().map(|h| ("idea", *h, *h)).collect();
    let ids = crate::ripples::ripple_write(&s.soup("kb"), "run-h", "DM-1", "sonnet\u{1b}[5m", &recs);
    let check = |what: &str, out: &str| {
        assert!(hidden_in(out).is_empty(), "{what}: {:?}\n{out}", hidden_in(out));
        assert!(!out.lines().any(|l| l.starts_with("Done: 9") || l.starts_with("approve: cauldron") || l.starts_with("review >> a")), "{what}: a line a record wrote:\n{out}");
    };
    // --text.
    let o = s.run(&["ripples", "review", "--env", "dev", "--cauldron", "kb", "--text"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    check("--text", &o.out);
    assert!(o.out.contains("\\u{202E}evorppa\\u{202C}") && o.out.contains("\\u{1B}[2J") && o.out.contains("\\u{9B}31mC1") && o.out.contains("\\u{200B}zero"), "{}", o.out);
    assert!(o.out.contains("sonnet\\u{1B}[5m"), "the model name too: {}", o.out);
    assert_eq!(o.out.lines().filter(|l| l.starts_with("  approve: ")).count(), 1, "{}", o.out);
    // JSON: the same strings, written as JSON escapes.
    for args in [
        vec!["ripples", "review", "--env", "dev", "--cauldron", "kb"],
        vec!["soup", "get", ids[2].as_str(), ids[4].as_str(), "--env", "dev", "--cauldron", "kb"],
        vec!["soup", "nodes", "--env", "dev", "--cauldron", "kb", "--limit", "100"],
    ] {
        let o = s.run(&args, &[]);
        assert_eq!(o.rc, 0, "{args:?}: {}", o.err);
        assert!(hidden_in(&o.out).is_empty(), "{args:?}: {:?}", hidden_in(&o.out));
        assert!(o.out.contains("\\u202e"), "{args:?}: {}", o.out);
        let v = o.json();
        assert!(v.to_string().contains('\u{202e}'), "{args:?}: the JSON still means the stored text");
    }
    // The interactive pass, at a terminal.
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let o = s.on_tty(&["ripples", "review", "--interactive", "--env", "dev", "--cauldron", "kb"], &pids, &["s"], Some("\nreview >> "));
    assert_eq!(o.rc, 0, "{}", o.out);
    check("--interactive", &o.out.replace("\r", ""));
    assert_eq!(o.out.lines().filter(|l| l.starts_with("review >> ")).count(), 1, "one prompt, and no record wrote one:\n{}", o.out);
    assert!(o.out.contains("\\u{202E}evorppa") && o.out.contains("line one review >> a"), "{}", o.out);
    svc.stop();
    // A dump's text: escaped at a terminal, verbatim to a pipe.
    let text = "a dump \u{1b}]0;title\u{7} with \u{202e}override\u{202c}\nand a second line\n";
    let mut c = s.command(&["soup", "dump", "-", "--env", "dev", "--cauldron", "kb"], &[]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    let d = c.wait_with_output().unwrap();
    assert!(d.status.success(), "{}", String::from_utf8_lossy(&d.stderr));
    let id: Value = serde_json::from_slice(&d.stdout).unwrap();
    let id = id["dump_id"].as_str().unwrap().to_string();
    let o = s.on_tty(&["soup", "dump-get", &id, "--env", "dev", "--cauldron", "kb", "--raw"], &[], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let shown = o.out.replace("\r", "");
    assert!(hidden_in(&shown).is_empty(), "{:?}: {shown}", hidden_in(&shown));
    assert!(shown.contains("\\u{1B}]0;title\\u{7}") && shown.contains("\\u{202E}override") && shown.contains("\nand a second line"), "{shown}");
    let piped = s.command(&["soup", "dump-get", &id, "--env", "dev", "--cauldron", "kb", "--raw"], &[]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(piped.stdout, text.as_bytes(), "a pipe gets the stored bytes");
}

/// **One key never half-decides a group.** With records marked approve and
/// reject, `w` asks for the reason before anything is written; ending the
/// pass there (end of input) writes nothing, and the group's decision, when
/// it is made, is one audited act.
#[test]
fn rv6_the_interactive_pass_decides_a_group_whole_or_not_at_all() {
    let s = Scratch::new("rv6", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y")]);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let args = ["ripples", "review", "--interactive", "--env", "dev", "--cauldron", "kb"];
    // Mark 1 approve, 2 reject, write: the reason is asked; the input ends
    // there (ctrl-D at the terminal).
    let o = s.on_tty(&args, &pids, &["1", "2", "2", "w", "\u{4}"], Some(">> "));
    assert!(o.out.contains("reason for the rejection"), "{}", o.out);
    let q = s.queue(&[]);
    assert_eq!((q["counts"]["unreviewed"].clone(), q["counts"]["approved"].clone()), (json!(2), json!(0)), "nothing of the group was written: {q}");
    assert!(s.calls().iter().all(|l| l["client"] != "cli"), "nothing was sent: {:?}", s.calls());
    // Answered: one act, both decisions.
    let o = s.on_tty(&args, &pids, &["1", "2", "2", "w", "dup"], Some(">> "));
    assert!(o.out.contains("approved 1, rejected 1"), "{}", o.out);
    let decided: Vec<Value> = s.calls().into_iter().filter(|l| l["client"] == "cli" && l["tool"].as_str().is_some_and(|t| t.starts_with("soup_"))).collect();
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!((decided[0]["tool"].clone(), decided[0]["counts"]["approved"].clone(), decided[0]["counts"]["rejected"].clone()), (json!("soup_approve_and_reject"), json!(1), json!(1)), "{}", decided[0]);
    let q = s.queue(&[]);
    assert_eq!((q["counts"]["unreviewed"].clone(), q["counts"]["approved"].clone(), q["counts"]["rejected"].clone()), (json!(0), json!(1), json!(1)), "{q}");
    let _ = a;
    svc.stop();
}

// ------------------------------------------------------------- clusters

const CLUSTER_BOUNDARY: &str = "Inside: everything that gets a release to the people who use it, end to end.";

fn cluster(label: &str, claims: &[&str]) -> Value {
    let edges: Vec<Value> = claims.iter().map(|r| json!({"kind": "claims", "to": r})).collect();
    json!({"id": label, "type": "cluster", "handle": "Shipping", "boundary": CLUSTER_BOUNDARY, "edges": edges})
}

/// While the service holds the soup, an import holding a cluster and a
/// `cauldron soup claim` are both sent to it and land through it.
#[test]
fn k1_a_cluster_import_and_a_claim_are_sent_to_the_service_that_holds_the_soup() {
    let s = Scratch::new("k1", &["kb"]);
    let ex = s.write("example.json", &json!(example()));
    assert_eq!(s.import("kb", &ex, &[], &[]).rc, 0);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    assert!(!lock_free(&s, "kb"), "the service holds the soup");
    let o = s.import("kb", &s.write("k.json", &json!([cluster("k1", &["R-1"])])), &["--remint"], &pids);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["id_map"]["k1"], "CL-1", "{}", o.out);
    assert_eq!(o.json()["forwarded_to"]["pid"], json!(svc.pid()));
    let o = s.run(&["soup", "claim", "CL-1", "R-2", "--env", "dev", "--cauldron", "kb"], &pids);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    assert_eq!((o.json()["outcome"].clone(), o.json()["forwarded_to"]["pid"].clone()), (json!("claimed"), json!(svc.pid())), "{}", o.out);
    assert!(s.calls().iter().any(|l| l["tool"] == "soup_claim" && l["outcome"] == "claimed" && l["client"] == "cli"), "{:?}", s.calls());
    svc.stop();
}

/// THE OLDER SERVICE. With `$CAULDRON_OLDER_MCP_BIN` naming a 0.1.14
/// `cauldron-mcp`: on a store holding clusters it opens the soup, reads and
/// writes it, and shows no cluster (its `soup_region` answers `clusters:
/// []`, `soup_get CL-1` answers it missing); its `soup_changes` lists the
/// earlier cluster writes as changes, by id. Skipped without the variable.
#[test]
fn k2_a_store_with_clusters_opens_reads_and_writes_under_the_older_service() {
    let Some(older) = std::env::var_os("CAULDRON_OLDER_MCP_BIN") else {
        eprintln!("k2: skipped: set CAULDRON_OLDER_MCP_BIN to a 0.1.14 `cauldron-mcp` to run it");
        return;
    };
    let s = Scratch::new("k2", &["kb"]);
    let ex = s.write("example.json", &json!(example()));
    assert_eq!(s.import("kb", &ex, &[], &[]).rc, 0);
    assert_eq!(s.import("kb", &s.write("k.json", &json!([cluster("k1", &["R-1"])])), &["--remint"], &[]).rc, 0);
    s.grant("kb");
    let svc = Service::start_bin(&s, Path::new(&older));
    let st = svc.call(&s, "soup_status", json!({"cauldron": "kb"}));
    assert_eq!(st["isError"], json!(false), "{st}");
    assert!(st["structuredContent"]["counts"].get("cluster").is_none(), "{st}");
    let r = svc.call(&s, "soup_region", json!({"cauldron": "kb", "id": "R-1"}));
    assert_eq!(r["structuredContent"]["clusters"], json!([]), "{r}");
    let g = svc.call(&s, "soup_get", json!({"cauldron": "kb", "ids": ["CL-1"]}));
    assert_eq!(g["structuredContent"]["missing"], json!(["CL-1"]), "{g}");
    let ch = svc.call(&s, "soup_changes", json!({"cauldron": "kb", "since": format!("v1.{}.0.0", st["structuredContent"]["cursor"].as_str().unwrap().split('.').nth(1).unwrap())}));
    let text = ch["structuredContent"].to_string();
    assert!(text.contains("CL-1"), "the older change feed lists the cluster write: {ch}");
    let w = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "The older service still writes")]}));
    assert_eq!(w["isError"], json!(false), "{w}");
    let k = svc.call(&s, "soup_claim", json!({"cauldron": "kb", "cluster": "CL-1", "region": "R-2"}));
    assert_eq!(k["structuredContent"]["refused"]["code"], "unsupported", "{k}");
    svc.stop();
    // This release reads the cluster and its claim afterwards.
    let o = s.run(&["soup", "get", "CL-1", "--env", "dev", "--cauldron", "kb"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(o.json()["records"][0]["claims"][0]["region"], "R-1", "{}", o.out);
}

// --------------------------------------------------------------------- B3

/// A `soup_changes` wait sent straight to the door on a connection the
/// caller keeps open, as session `session`.
fn hold_wait(s: &Scratch, svc: &Service, since: &str, session: &str, secs: u64) -> TcpStream {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "soup_changes", "arguments": {
        "cauldron": "kb", "since": since, "wait_secs": secs,
        "_ctx": {"client": "test", "cwd": s.project().display().to_string(), "session_id": session}}}})
    .to_string();
    let mut c = TcpStream::connect(svc.door).unwrap();
    write!(
        c,
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        svc.door.port(),
        s.secret(),
        body.len()
    )
    .unwrap();
    c
}

/// B3: every wait slot held, a session's `soup_changes` wait through the
/// bridge is not handed back at once as `not-waited`: the bridge sleeps the
/// service's `retry_after_secs`, asks again, and answers once it was given a
/// slot and the wait ran.
#[test]
fn b3_the_bridge_backs_off_when_every_wait_slot_is_held() {
    let s = Scratch::new("b3", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");
    let svc = Service::start(&s);
    let cur = svc.call(&s, "soup_status", json!({"cauldron": "kb"}))["structuredContent"]["cursor"].as_str().unwrap().to_string();
    let mut held: Vec<TcpStream> = (0..16).map(|i| hold_wait(&s, &svc, &cur, &format!("h{i}"), 3)).collect();
    // Keep all sixteen held: one answered at once (racing the others in)
    // is sent again.
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let mut answered = 0;
        for (i, c) in held.iter_mut().enumerate() {
            c.set_nonblocking(true).unwrap();
            let early = c.peek(&mut [0u8; 1]).is_ok();
            c.set_nonblocking(false).unwrap();
            if early {
                answered += 1;
                *c = hold_wait(&s, &svc, &cur, &format!("h{i}"), 3);
            }
        }
        if answered == 0 {
            break;
        }
    }
    let full = svc.call(&s, "soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 2}));
    assert_eq!(full["structuredContent"]["waited"]["ended"], "not-waited", "the sixteen hold every slot: {full}");
    let mut b = Bridge::start(&s, &s.project(), &[], &[("CLAUDE_CODE_SESSION_ID", "sess-b3")], &[svc.pid()]);
    init(&mut b);
    let t0 = Instant::now();
    let r = b.call(1, "soup_changes", json!({"cauldron": "kb", "since": cur, "wait_secs": 8}));
    let took = t0.elapsed();
    assert_eq!(r["isError"], json!(false), "{r}");
    assert_eq!(r["structuredContent"]["waited"]["ended"], "timeout", "the bridge waited for a slot, then waited: {r}");
    assert!(took >= Duration::from_secs(5) && took < Duration::from_secs(20), "{took:?}");
    drop(held);
    b.finish();
    svc.stop();
}

// --------------------------------------------------------------------- B4

impl Service {
    /// SIGKILL: the service dies at once, mid-call.
    fn kill_hard(mut self) {
        Command::new("/bin/kill").args(["-KILL", &self.child.id().to_string()]).status().unwrap();
        let _ = self.child.wait();
    }
}

/// B4: the service killed (SIGKILL) while a call is in flight reaches the
/// session as the bridge's typed `service_down` refusal with the start
/// command, never as a JSON-RPC error: mid-wait (a read: nothing was
/// written) and mid-put (a write: whether it landed is not known, and the
/// refusal says how to check, with `soup_changes` since the last cursor).
#[test]
fn b4_a_service_killed_mid_call_is_a_typed_refusal_not_a_protocol_error() {
    let s = Scratch::new("b4", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");

    // Mid-wait.
    let svc = Service::start(&s);
    let mut b = Bridge::start(&s, &s.project(), &[], &[("CLAUDE_CODE_SESSION_ID", "sess-b4")], &[svc.pid()]);
    init(&mut b);
    let cur = b.call(1, "soup_status", json!({}))["structuredContent"]["cursor"].as_str().unwrap().to_string();
    let w = b.stdin.as_mut().unwrap();
    writeln!(w, "{}", json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "soup_changes", "arguments": {"since": cur, "wait_secs": 30}}})).unwrap();
    w.flush().unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let t0 = Instant::now();
    svc.kill_hard();
    let mut line = String::new();
    b.stdout.read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert!(v.get("error").is_none(), "a protocol error, not a typed refusal: {v}");
    assert_eq!(v["id"], 2, "{v}");
    let (code, msg) = refused(&v["result"]);
    assert_eq!(code, "service_down", "{msg}");
    assert!(msg.contains("`cauldron --env dev start mcp`"), "{msg}");
    assert!(msg.contains("while this call was in flight") && msg.contains("only reads, so nothing was written"), "{msg}");
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    // And the next call is the ordinary `service_down`.
    let (code, _) = refused(&b.call(3, "soup_status", json!({})));
    assert_eq!(code, "service_down");
    b.finish();

    // Mid-put: a batch big enough that the kill lands while the service
    // has it (made bigger until it does).
    let mut n = 2000;
    loop {
        let svc = Service::start(&s);
        let mut b = Bridge::start(&s, &s.project(), &[], &[("CLAUDE_CODE_SESSION_ID", "sess-b4")], &[svc.pid()]);
        init(&mut b);
        let records: Vec<Value> = (0..n).map(|i| task(&format!("t{i}"), &format!("Killed mid-put task {n} {i}"))).collect();
        let w = b.stdin.as_mut().unwrap();
        writeln!(w, "{}", json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "soup_put", "arguments": {"remint": true, "records": records}}})).unwrap();
        w.flush().unwrap();
        // Kill it the moment the service has the write in hand: its row is
        // on the spine and not yet cleared.
        let spine = s.soup("kb").join("spine.db");
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(20) {
            let open: i64 = spine::offline::read_only_connection(&spine)
                .ok()
                .and_then(|c| c.query_row("SELECT count(*) FROM transactions WHERE cleared = 0", [], |r| r.get(0)).ok())
                .unwrap_or(0);
            if open > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        svc.kill_hard();
        let mut line = String::new();
        b.stdout.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("error").is_none(), "a protocol error, not a typed refusal: {v}");
        if v["result"]["isError"] == json!(false) {
            assert!(n < 64_000, "the put always finished before the kill");
            n *= 2;
            b.finish();
            continue;
        }
        let (code, msg) = refused(&v["result"]);
        assert_eq!(code, "service_down", "{msg}");
        assert!(msg.contains("`cauldron --env dev start mcp`") && msg.contains("while this call was in flight"), "{msg}");
        assert!(msg.contains("WHETHER THIS WRITE LANDED IS NOT KNOWN"), "{msg}");
        assert!(msg.contains("soup_changes") && msg.contains("since") && msg.contains("batch_already_landed"), "{msg}");
        b.finish();
        break;
    }
}

// --------------------------------------------------------------------- F3

/// `soup import-corpus` while the service holds the cauldron: sent to it as
/// one `soup_put` under `--remint`, landed through it, printed with
/// `forwarded_to` and the `corpus` report; the same batch again is refused
/// through it as `batch-already-landed`.
#[test]
fn f3_a_corpus_import_is_sent_to_the_service_that_holds_the_soup() {
    let s = Scratch::new("f3", &["kb"]);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let ex = s.write("example.json", &json!(example()));
    assert_eq!(s.import("kb", &ex, &[], &pids).rc, 0);
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    assert!(!lock_free(&s, "kb"), "the service holds the soup");
    let before = s.node_count("kb");
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/it/fixtures/corpus-v5/records");
    let args = ["soup", "import-corpus", input.to_str().unwrap(), "--env", "dev", "--cauldron", "kb"];
    let o = s.run(&args, &pids);
    assert_eq!(o.rc, 0, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!(d["outcome"], "landed", "{d}");
    assert_eq!(d["forwarded_to"], json!({"service": "mcp", "pid": svc.pid()}));
    assert_eq!(d["corpus"]["carried"]["nodes"], 4, "{d}");
    assert_eq!(d["corpus"]["id_map"].as_object().map(|m| m.len()), Some(7), "{d}");
    assert_eq!(s.node_count("kb"), before + 7);
    let o = s.run(&args, &pids);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    let d = o.json();
    assert_eq!((d["refusal"].clone(), d["forwarded_to"]["pid"].clone()), (json!("batch-already-landed"), json!(svc.pid())), "{d}");
    assert_eq!(s.node_count("kb"), before + 7);
    svc.stop();
}

// --------------------------------------------------------------------- B5

/// **One server per cauldron.** `cauldron mcp config --cauldron kb` prints
/// a `.mcp.json` entry (and nothing else on stdout) whose command is this
/// cauldron by its absolute path and whose args pin the bridge to `kb`;
/// that entry, run as written, is a server for `kb` alone: a call naming no
/// cauldron reaches `kb` from any directory, a call naming `kb` too, and a
/// call naming another granted cauldron is refused as `cauldron_pinned`
/// and never reaches the service (a write lands nothing), whatever
/// `$CAULDRON_CAULDRON` says; with the service down the pin still answers
/// first. Without `--cauldron`, `$CAULDRON_CAULDRON` only chooses a
/// default: a call may name another cauldron. The verb refuses no
/// `--cauldron` and a cauldron the environment does not have, and names the
/// grant command for one not granted.
#[test]
fn b5_a_server_pinned_to_one_cauldron_refuses_every_other() {
    let s = Scratch::new("b5", &["kb", "other", "ungranted"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    assert_eq!(s.import("other", &s.write("example2.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");
    soup_door::grants::grant(&s.env(), "other", &Scope::ALL, &[], "tty").unwrap();

    // The entry.
    let o = s.run(&["mcp", "config", "--cauldron", "kb", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let entry = o.json();
    let server = &entry["mcpServers"]["cauldron-kb"];
    assert_eq!(server["command"], json!(CAULDRON), "{entry}");
    let args: Vec<String> = server["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
    assert_eq!(args, ["mcp", "stdio", "--env", "dev", "--cauldron", "kb"], "{entry}");
    assert_eq!(entry["mcpServers"].as_object().unwrap().len(), 1, "{entry}");
    assert!(o.err.contains("cauldron_pinned") && !o.err.contains("not granted"), "{}", o.err);
    // Its refusals, and the note for a cauldron not granted.
    let o = s.run(&["mcp", "config", "--env", "dev"], &[]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("--cauldron"), "{}", o.err);
    let o = s.run(&["mcp", "config", "--cauldron", "nope", "--env", "dev"], &[]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("no cauldron `nope`") && o.out.is_empty(), "{}", o.err);
    let o = s.run(&["mcp", "config", "--cauldron", "ungranted", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(o.err.contains("not granted") && o.err.contains("cauldron mcp grant ungranted"), "{}", o.err);

    // The entry, run as written, from a directory outside every project,
    // with $CAULDRON_CAULDRON naming the other cauldron.
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let pinned: Vec<&str> = args[2..].iter().map(String::as_str).collect();
    let mut b = Bridge::start_bare(&s, &s.0.join("work"), &pinned, &[("CAULDRON_CAULDRON", "other")], &pids);
    let i = init(&mut b);
    assert!(i["result"]["instructions"].as_str().unwrap().contains("pinned to cauldron `kb`"), "{i}");
    let st = b.call(1, "soup_status", json!({}));
    assert_eq!(st["structuredContent"]["cauldron"], "kb", "{st}");
    let st = b.call(2, "soup_status", json!({"cauldron": "kb"}));
    assert_eq!(st["structuredContent"]["cauldron"], "kb", "{st}");
    let before = (s.node_count("kb"), s.node_count("other"), s.calls().len());
    for (n, (tool, args)) in [
        ("soup_status", json!({"cauldron": "other"})),
        ("soup_search", json!({"cauldron": "other", "query": "import"})),
        ("soup_put", json!({"cauldron": "other", "remint": true, "records": [task("t", "Must not land")]})),
        ("soup_link", json!({"cauldron": " other ", "from": "DEC-1", "kind": "ref", "to": "T-1"})),
        ("soup_dump_session", json!({"cauldron": "other"})),
    ]
    .into_iter()
    .enumerate()
    {
        let (code, msg) = refused(&b.call(10 + n as u64, tool, args));
        assert_eq!(code, "cauldron_pinned", "{tool}: {msg}");
        assert!(msg.contains("`kb`") && msg.contains("`other`") && msg.contains("Nothing was read or written"), "{msg}");
    }
    assert_eq!((s.node_count("kb"), s.node_count("other"), s.calls().len()), before, "nothing reached the service");
    // A write naming no cauldron lands in kb.
    let put = b.call(20, "soup_put", json!({"remint": true, "records": [task("t", "Pinned write")]}));
    assert_eq!(put["structuredContent"]["cauldron"], "kb", "{put}");
    assert_eq!(s.node_count("kb"), before.0 + 1);
    b.finish();

    // Not pinned: $CAULDRON_CAULDRON is a default, and a call may name another.
    let mut b = Bridge::start(&s, &s.0.join("work"), &[], &[("CAULDRON_CAULDRON", "kb")], &pids);
    init(&mut b);
    let st = b.call(1, "soup_status", json!({}));
    assert_eq!(st["structuredContent"]["cauldron"], "kb", "{st}");
    let st = b.call(2, "soup_status", json!({"cauldron": "other"}));
    assert_eq!(st["structuredContent"]["cauldron"], "other", "{st}");
    b.finish();
    svc.stop();

    // With the service down the pin still answers first.
    let mut b = Bridge::start_bare(&s, &s.0.join("work"), &pinned, &[], &[]);
    init(&mut b);
    let (code, _) = refused(&b.call(1, "soup_status", json!({"cauldron": "other"})));
    assert_eq!(code, "cauldron_pinned");
    let (code, _) = refused(&b.call(2, "soup_status", json!({})));
    assert_eq!(code, "service_down");
    let (rc, _, err) = b.finish();
    assert_eq!(rc, 0, "{err}");
}

/// `cauldron soup link | unlink | cite` while the `mcp` service holds the
/// soup: each is sent to it as `soup_link`, `soup_unlink`, `soup_cite`,
/// lands through it (`forwarded_to`), and is audited as the CLI's; a
/// refusal comes back with its reasons and exit 1.
#[test]
fn k3_link_unlink_and_cite_are_sent_to_the_service_that_holds_the_soup() {
    let s = Scratch::new("k3", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [task("t", "Opens the writer")]}));
    assert_eq!(r["isError"], json!(false), "{r}");
    assert!(!lock_free(&s, "kb"), "the service holds the soup");
    let kb = ["--env", "dev", "--cauldron", "kb"];
    let soup = |args: &[&str]| {
        let mut a = vec!["soup"];
        a.extend_from_slice(args);
        a.extend_from_slice(&kb);
        s.run(&a, &pids)
    };
    for (args, want, tool) in [
        (vec!["link", "T-1", "ref", "M-1"], "linked", "soup_link"),
        (vec!["unlink", "T-1", "ref", "M-1"], "unlinked", "soup_unlink"),
        (vec!["cite", "T-1", "--add", r#"{"kind":"url","url":"https://example.com/t"}"#], "cited", "soup_cite"),
        (vec!["cite", "T-1", "--remove", "0"], "uncited", "soup_cite"),
    ] {
        let o = soup(&args);
        assert_eq!(o.rc, 0, "{args:?}: {}\n{}", o.out, o.err);
        assert_eq!(o.json()["outcome"], want, "{}", o.out);
        assert_eq!(o.json()["forwarded_to"]["pid"], json!(svc.pid()), "{}", o.out);
        assert!(s.calls().iter().any(|l| l["tool"] == tool && l["outcome"] == want && l["client"] == "cli"), "{tool}: {:?}", s.calls());
    }
    let o = soup(&["unlink", "T-1", "in_region", "R-2"]);
    assert_eq!(o.rc, 1, "{}\n{}", o.out, o.err);
    assert_eq!(o.json()["links"][0]["refused"]["reason"], "required-edge", "{}", o.out);
    svc.stop();
}

// ------------------------------------------------------------- owner acts

impl Scratch {
    /// [`Scratch::run`] with `vars` set on the child (no terminal).
    fn run_with(&self, args: &[&str], pids: &[u32], vars: &[(&str, &str)]) -> Out {
        let mut c = self.command(args, pids);
        for (k, v) in vars {
            c.env(k, v);
        }
        let o = c.stdin(Stdio::null()).output().expect("spawn cauldron");
        Out { rc: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() }
    }
    /// The grant row of `c`, if any.
    fn grant_row(&self, c: &str) -> Option<Value> {
        let g: Value = serde_json::from_str(&std::fs::read_to_string(self.mcp_dir().join("grants.json")).ok()?).ok()?;
        g["grants"].as_array()?.iter().find(|x| x["cauldron"] == c).cloned()
    }
    /// One record, read back.
    fn record(&self, id: &str) -> Value {
        let o = self.run(&["soup", "get", id, "--env", "dev", "--cauldron", "kb"], &[]);
        o.json()["records"][0].clone()
    }
    /// One line to the running service's seam, answered.
    fn seam_line(&self, line: &str) -> String {
        let t: Value = serde_json::from_str(&std::fs::read_to_string(self.data().join("seams.json")).unwrap()).unwrap();
        let addr: SocketAddr = t["seams"]["mcp"]["addr"].as_str().unwrap().parse().unwrap();
        let mut c = TcpStream::connect(addr).unwrap();
        writeln!(c, "{line}").unwrap();
        let mut answer = String::new();
        BufReader::new(c).read_line(&mut answer).unwrap();
        answer.trim().to_string()
    }
    fn key(&self, f: &str) -> String {
        std::fs::read_to_string(self.mcp_dir().join(f)).unwrap().trim().to_string()
    }
    /// The owner-acts setting file, if any.
    fn owner_acts_file(&self) -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(self.root().join("owner-acts.json")).ok()?).ok()
    }
}

impl Service {
    /// One `tools/call` straight to the door with this `_ctx`.
    fn call_as(&self, s: &Scratch, tool: &str, mut args: Value, client: &str, session: Option<&str>) -> Value {
        let mut ctx = json!({"client": client, "cwd": s.project().display().to_string()});
        if let Some(id) = session {
            ctx["session_id"] = json!(id);
        }
        args["_ctx"] = ctx;
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).to_string();
        let mut c = TcpStream::connect(self.door).unwrap();
        write!(
            c,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.door.port(),
            s.secret(),
            body.len()
        )
        .unwrap();
        let mut raw = String::new();
        c.read_to_string(&mut raw).unwrap();
        let v: Value = serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap();
        v["result"].clone()
    }
}

/// A tool call's structured answer, or its refusal's code.
fn answer_of(r: &Value) -> Value {
    r.get("structuredContent").cloned().unwrap_or_else(|| r.clone())
}

fn refusal_code(r: &Value) -> String {
    r["structuredContent"]["refused"]["code"].as_str().map(str::to_string).unwrap_or_else(|| r.to_string())
}

const OPEN: (&str, &str) = ("CAULDRON_OWNER_ACTS", "open");

/// **OA1: every owner act is refused without a terminal by default, and the
/// variable allows it, recorded `by: "agent"` everywhere.** With nothing
/// chosen, `mcp grant`, `soup approve`, `ripples approve | reject` and
/// `ripples on` each refuse without a terminal, name the command and how the
/// owner allows it, and write nothing. With `CAULDRON_OWNER_ACTS=open` each
/// runs: the grant row, the decided records, the answers, the audit log and
/// the automatic journal say `agent`; `ripples on` asks nothing and prints
/// what it authorised. A variable naming one act opens only that one; one
/// that does not parse opens nothing and says so.
#[test]
fn oa1_each_act_needs_a_terminal_by_default_and_the_variable_opens_it() {
    let s = Scratch::new("oa1", &["kb", "other"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y")]);
    let b = crate::ripples::ripple_write(&s.soup("kb"), "run-b", "DM-2", "sonnet", &[("idea", "three", "z")]);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let acts: Vec<Vec<&str>> = vec![
        vec!["mcp", "grant", "other", "--env", "dev"],
        vec!["soup", "approve", a[0].as_str(), "--env", "dev", "--cauldron", "kb"],
        vec!["ripples", "approve", a[1].as_str(), "--env", "dev", "--cauldron", "kb"],
        vec!["ripples", "reject", "--dump", "DM-2", "--env", "dev", "--cauldron", "kb"],
        vec!["ripples", "on", "kb", "--env", "dev"],
    ];
    for args in &acts {
        let o = s.run(args, &pids);
        assert_ne!(o.rc, 0, "{args:?}: {}", o.out);
        assert!(o.err.contains("REFUSED:") && o.err.contains("owner's act at a terminal") && o.err.contains("unless the owner has let agents"), "{args:?}: {}", o.err);
        assert!(o.err.contains("cauldron owner-acts open --only") && o.err.contains("CAULDRON_OWNER_ACTS="), "{args:?}: {}", o.err);
    }
    assert!(s.grant_row("other").is_none(), "nothing granted");
    assert_eq!(s.queue(&[])["counts"]["unreviewed"], 3, "nothing decided");
    assert!(!s.data().join("ripples/auto/kb.json").exists(), "nothing turned on");
    // One act open: only that one.
    let o = s.run_with(&acts[0], &pids, &[("CAULDRON_OWNER_ACTS", "approve")]);
    assert_ne!(o.rc, 0, "grant is not opened by `approve`: {}", o.out);
    // A value that does not parse opens nothing and says why.
    let o = s.run_with(&acts[1], &pids, &[("CAULDRON_OWNER_ACTS", "yes")]);
    assert!(o.rc != 0 && o.err.contains("CAULDRON_OWNER_ACTS=yes") && o.err.contains("it opens nothing"), "{}", o.err);
    // The variable: each runs, as an agent.
    let o = s.run_with(&acts[0], &pids, &[OPEN]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["by"], "agent");
    assert_eq!(s.grant_row("other").unwrap()["by"], "agent");
    // Approving and rejecting are carried out by the mcp service, which
    // takes an agent's decision only where the persisted setting opens
    // `approve`: the variable alone is refused before anything is sent, and
    // an `ADECIDE` sent straight to the service is refused there too.
    for args in &acts[1..4] {
        let o = s.run_with(args, &pids, &[OPEN]);
        assert!(o.rc != 0 && o.err.contains("which takes an agent's decision only where the owner's persisted owner_acts setting opens `approve`") && o.err.contains("Nothing was sent"), "{args:?}: {}", o.err);
    }
    let line = s.seam_line(&format!("ADECIDE {} {}", s.key("approve.secret"), json!({"cauldron": "kb", "action": "approve", "ids": [a[0]]})));
    assert!(line.contains("\"owner_act_closed\"") && line.contains("persisted owner_acts setting"), "{line}");
    assert_eq!(s.queue(&[])["counts"]["unreviewed"], 3, "nothing decided");
    let o = s.run_with(&["owner-acts", "open", "--only", "approve"], &[], &[OPEN]);
    assert_eq!((o.rc, o.json()["how"].clone()), (0, json!("env")), "{}", o.err);
    let o = s.run_with(&acts[1], &pids, &[OPEN]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!((o.json()["by"].clone(), o.json()["approved"].clone()), (json!("agent"), json!([a[0]])), "{}", o.out);
    let o = s.run_with(&acts[2], &pids, &[("CAULDRON_OWNER_ACTS", "approve")]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert!(o.err.contains("by: agent"), "{}", o.err);
    let o = s.run_with(&acts[3], &pids, &[OPEN]);
    assert_eq!((o.rc, o.json()["rejected"].clone()), (0, json!([b[0]])), "{}", o.err);
    for id in [&a[0], &a[1]] {
        assert_eq!(s.record(id)["meta"]["approved"]["by"], "agent", "{id}");
    }
    assert_eq!(s.record(&b[0])["meta"]["rejected"]["by"], "agent");
    let audit = s.calls();
    for t in ["soup_approve", "soup_reject"] {
        assert!(audit.iter().any(|l| l["tool"] == t && l["client"] == "cli" && l["by"] == "agent"), "{t}: {audit:?}");
    }
    let o = s.run_with(&acts[4], &pids, &[OPEN]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert!(o.out.contains("No terminal: no question asked") && !o.out.contains("[y/N]"), "{}", o.out);
    let v = printed_json(&o.out);
    assert_eq!((v["outcome"].clone(), v["by"].clone()), (json!("on"), json!("agent")), "{v}");
    assert!(v["authorised"]["what"].as_str().unwrap().contains("What is sent where"), "{v}");
    let journal = std::fs::read_to_string(s.data().join("ripples/auto.jsonl")).unwrap();
    let on: Value = journal.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find(|l| l["event"] == "turned_on").expect("a turned_on line");
    assert_eq!((on["by"].clone(), on["cauldron"].clone()), (json!("agent"), json!("kb")), "{on}");
    // The status views show who turned it on and the effective setting.
    let o = s.run(&["ripples", "status", "--env", "dev", "--cauldron", "kb"], &pids);
    let st = o.json();
    let kb = st["owner_acts"]["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!((kb["turned_on_by"]["by"].clone(), kb["setting"].clone()), (json!("agent"), json!("approve")), "{st}");
    svc.stop();
}

/// **OA2: `owner-acts open` persists the choice; a cauldron's own choice
/// wins both ways; `--only` opens only what it lists; `unset` hands the
/// cauldron back to the root.** Opening without a terminal and without the
/// variable is refused, nothing written; closing runs anywhere. Every
/// choice records when and how it was set, and `show`, `mcp status`,
/// `ripples status` and `status` say what is in effect and from where.
#[test]
fn oa2_the_setting_persists_and_a_cauldrons_choice_wins_both_ways() {
    let s = Scratch::new("oa2", &["kb", "k2"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    assert_eq!(s.import("k2", &s.write("example2.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y"), ("idea", "three", "z")]);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let approve = |id: &str| s.run(&["soup", "approve", id, "--env", "dev", "--cauldron", "kb"], &pids);
    // No setting is `terminal`.
    let v = s.run(&["owner-acts", "show", "--env", "dev"], &[]).json();
    assert_eq!(v["summary"]["root"], "terminal");
    assert!(v["cauldrons"].as_array().unwrap().iter().all(|c| c["setting"] == "terminal"), "{v}");
    // Opening needs a terminal or the variable.
    let o = s.run(&["owner-acts", "open"], &[]);
    assert!(o.rc != 0 && o.err.contains("REFUSED: letting agents act for the owner is the owner's choice") && o.err.contains("cauldron owner-acts open --only grant,approve,ripples"), "{}", o.err);
    let o = s.run_with(&["owner-acts", "open"], &[], &[("CAULDRON_OWNER_ACTS", "approve")]);
    assert!(o.rc != 0 && o.err.contains("it would open grant,ripples"), "the variable must open each act opened: {}", o.err);
    assert!(s.owner_acts_file().is_none(), "nothing written");
    // How a choice was set is the command's to record, never an argument's:
    // `--how install` is refused, with the variable and at a terminal alike.
    let o = s.run_with(&["owner-acts", "open", "--only", "approve", "--how", "install"], &[], &[("CAULDRON_OWNER_ACTS", "approve")]);
    assert!(o.rc != 0 && o.err.contains("`--how` is not an option") && o.err.contains("cauldron install --owner-acts"), "{}", o.err);
    let o = s.on_tty(&["owner-acts", "terminal", "--how=install"], &[], &[], None);
    assert!(o.rc != 0 && o.out.contains("`--how` is not an option"), "{}", o.out);
    assert!(s.owner_acts_file().is_none(), "nothing written");
    // At a terminal, --only approve: approve opens, grant does not.
    let o = s.on_tty(&["owner-acts", "open", "--only", "approve"], &[], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let v = printed_json(&o.out);
    assert_eq!((v["setting"].clone(), v["how"].clone(), v["scope"].clone()), (json!("approve"), json!("tty"), json!("root")), "{v}");
    assert!(v["note"].as_str().unwrap().contains("approve or reject what a ripple wrote"), "{v}");
    assert_eq!(s.owner_acts_file().unwrap()["root"]["how"], "tty");
    let o = approve(&a[0]);
    assert_eq!((o.rc, o.json()["by"].clone()), (0, json!("agent")), "{}", o.err);
    let o = s.run(&["mcp", "grant", "k2", "--env", "dev"], &[]);
    assert_ne!(o.rc, 0, "--only approve does not open grant: {}", o.out);
    // The cauldron's own choice closes it for kb alone (closing needs no terminal).
    let o = s.run(&["owner-acts", "terminal", "--cauldron", "kb", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!((o.json()["setting"].clone(), o.json()["how"].clone()), (json!("terminal"), json!("env")));
    let o = approve(&a[1]);
    assert!(o.rc != 0 && o.err.contains("owner's act at a terminal"), "kb's own choice wins: {}", o.err);
    let v = s.run(&["owner-acts", "show", "--env", "dev"], &[]).json();
    let by = |c: &str| v["cauldrons"].as_array().unwrap().iter().find(|x| x["cauldron"] == c).unwrap().clone();
    assert_eq!((by("kb")["from"].clone(), by("kb")["acts"]["approve"].clone()), (json!("cauldron"), json!("terminal")), "{v}");
    assert_eq!((by("k2")["from"].clone(), by("k2")["acts"]["approve"].clone()), (json!("root"), json!("root")), "{v}");
    // The other way: root closed, kb opened, by the variable.
    assert_eq!(s.run(&["owner-acts", "terminal"], &[]).rc, 0);
    let o = s.run_with(&["owner-acts", "open", "--only", "approve", "--cauldron", "kb", "--env", "dev"], &[], &[("CAULDRON_OWNER_ACTS", "approve")]);
    assert_eq!((o.rc, o.json()["how"].clone()), (0, json!("env")), "{}", o.err);
    let o = approve(&a[1]);
    assert_eq!((o.rc, o.json()["by"].clone()), (0, json!("agent")), "{}", o.err);
    // The status views.
    let m = s.run(&["mcp", "status", "--env", "dev"], &pids).json();
    let row = m["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!((row["owner_acts"]["setting"].clone(), row["owner_acts"]["from"].clone()), (json!("approve"), json!("cauldron")), "{m}");
    assert_eq!(m["owner_acts"]["root"], "terminal", "{m}");
    let st = s.run_with(&["status", "--json"], &[], &[("CAULDRON_ENV", "dev")]);
    let st: Value = serde_json::from_str(&st.out).unwrap_or_else(|e| panic!("{e}: {} {}", st.out, st.err));
    assert!(st["owner_acts"]["cauldrons"].as_array().unwrap().iter().any(|c| c["cauldron"] == "kb" && c["setting"] == "approve"), "{st}");
    let txt = s.run_with(&["status"], &[], &[("CAULDRON_ENV", "dev")]);
    assert!(txt.out.contains("owner acts: root terminal; open without a terminal: kb approve"), "{}", txt.out);
    // unset: the root's applies again.
    let o = s.run(&["owner-acts", "unset", "--cauldron", "kb", "--env", "dev"], &[]);
    assert_eq!((o.rc, o.json()["outcome"].clone()), (0, json!("unset")), "{}", o.err);
    assert_ne!(approve(&a[2]).rc, 0, "back to the root's `terminal`");
    // --env without --cauldron is refused (the root's choice covers every environment).
    assert_ne!(s.run(&["owner-acts", "open", "--env", "dev"], &[]).rc, 0);
    // A cauldron with no choice of its own starts from the root's, and the
    // answer names what it took over and that it stops following the root.
    assert_eq!(s.on_tty(&["owner-acts", "open"], &[], &[], None).rc, 0);
    let o = s.run_with(&["owner-acts", "open", "--only", "approve", "--cauldron", "k2", "--env", "dev"], &[], &[("CAULDRON_OWNER_ACTS", "approve")]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let v = o.json();
    // (`open` with no --only opens every act, `merge` included.)
    assert_eq!((v["setting"].clone(), v["copied_from_root"].clone()), (json!("open"), json!("grant,ripples,merge")), "{v}");
    assert!(v["copied_note"].as_str().unwrap().contains("no longer reaches it"), "{v}");
    let o = s.run_with(&["owner-acts", "open", "--only", "approve", "--cauldron", "k2", "--env", "dev"], &[], &[("CAULDRON_OWNER_ACTS", "approve")]);
    assert!(o.json().get("copied_from_root").is_none(), "its own choice now: nothing copied: {}", o.out);
    svc.stop();
}

/// **OA3: who decided is on every record and the reads filter on it.** A
/// decision at a terminal says `tty`, one under the open setting `agent`;
/// `soup nodes --review by-tty | by-agent` keeps only those, each listed
/// with `decided_by`, and the review queue counts both.
#[test]
fn oa3_by_is_recorded_on_each_record_and_the_reads_filter_on_it() {
    let s = Scratch::new("oa3", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y"), ("idea", "three", "z")]);
    s.grant("kb");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let o = s.on_tty(&["soup", "approve", &a[0], "--env", "dev", "--cauldron", "kb"], &pids, &[], None);
    assert_eq!(printed_json(&o.out)["by"], "tty", "{}", o.out);
    // An agent's decision needs the persisted setting (the service checks it).
    assert_eq!(s.run_with(&["owner-acts", "open", "--only", "approve"], &[], &[OPEN]).rc, 0);
    let o = s.run_with(&["soup", "approve", &a[1], "--env", "dev", "--cauldron", "kb"], &pids, &[OPEN]);
    assert_eq!(o.json()["by"], "agent", "{}", o.err);
    assert_eq!((s.record(&a[0])["meta"]["approved"]["by"].clone(), s.record(&a[1])["meta"]["approved"]["by"].clone()), (json!("tty"), json!("agent")));
    let list = |f: &str| -> Vec<(String, String)> {
        let v = s.run(&["soup", "nodes", "--env", "dev", "--cauldron", "kb", "--review", f, "--limit", "100"], &[]).json();
        v["nodes"].as_array().unwrap().iter().map(|n| (n["id"].as_str().unwrap().to_string(), n["decided_by"].as_str().unwrap_or("").to_string())).collect()
    };
    assert_eq!(list("by-tty"), vec![(a[0].clone(), "tty".to_string())]);
    assert_eq!(list("by-agent"), vec![(a[1].clone(), "agent".to_string())]);
    let o = s.run(&["soup", "nodes", "--env", "dev", "--cauldron", "kb", "--review", "by-someone"], &[]);
    assert!(o.rc != 0 && o.err.contains("by-agent"), "{}", o.err);
    let q = s.queue(&[]);
    assert_eq!((q["counts"]["decided_by"]["tty"].clone(), q["counts"]["decided_by"]["agent"].clone()), (json!(1), json!(1)), "{q}");
    // The same filter through the door.
    let r = svc.call(&s, "soup_nodes", json!({"cauldron": "kb", "review": "by-agent"}));
    let ids: Vec<Value> = answer_of(&r)["nodes"].as_array().unwrap().iter().map(|n| n["id"].clone()).collect();
    assert_eq!(ids, vec![json!(a[1])], "{r}");
    svc.stop();
}

/// **OA4: the MCP tools.** Closed (the default), `soup_approve` and
/// `soup_reject` are refused `owner_act_closed`, nothing written. Opened by
/// the owner's setting, they decide by ids, dump or run, `undo` takes a
/// decision back, and each decision is recorded `by: "agent"` with the
/// session, on the record, in the answer and in the audit. A ripple's
/// identity is refused under every setting, and no record write sets a
/// review flag whatever the setting.
#[test]
fn oa4_the_mcp_tools_are_refused_closed_and_decide_as_an_agent_when_open() {
    let s = Scratch::new("oa4", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y")]);
    let b = crate::ripples::ripple_write(&s.soup("kb"), "run-b", "DM-2", "sonnet", &[("idea", "three", "z")]);
    s.grant("kb");
    let svc = Service::start(&s);
    let call = |tool: &str, args: Value| svc.call_as(&s, tool, args, "claude-code", Some("sess-1"));
    // Closed.
    for (tool, args) in [("soup_approve", json!({"cauldron": "kb", "ids": [a[0]]})), ("soup_reject", json!({"cauldron": "kb", "dump": "DM-2"}))] {
        let r = call(tool, args);
        assert_eq!(r["isError"], json!(true), "{tool}: {r}");
        assert_eq!(refusal_code(&r), "owner_act_closed", "{tool}: {r}");
        assert!(r["content"][0]["text"].as_str().unwrap().contains("cauldron owner-acts open --only approve"), "{r}");
    }
    assert_eq!(s.queue(&[])["counts"]["unreviewed"], 3, "nothing written");
    // A ripple's identity is refused, open or not.
    let ripple = |s: &Scratch| svc.call_as(s, "soup_approve", json!({"cauldron": "kb", "ids": [a[0]]}), "ripples", None);
    assert_eq!(refusal_code(&ripple(&s)), "ripple_refused");
    // Open (the owner's persisted choice, made at a terminal).
    assert_eq!(s.on_tty(&["owner-acts", "open", "--only", "approve"], &[], &[], None).rc, 0);
    assert_eq!(refusal_code(&ripple(&s)), "ripple_refused", "a ripple never decides, whatever the setting");
    let r = call("soup_approve", json!({"cauldron": "kb", "run": "run-a"}));
    let v = answer_of(&r);
    assert_eq!((v["by"].clone(), v["session"].clone(), v["approved"].as_array().map(Vec::len)), (json!("agent"), json!("sess-1"), Some(2)), "{r}");
    let rec = s.record(&a[0]);
    assert_eq!((rec["meta"]["approved"]["by"].clone(), rec["meta"]["approved"]["session"].clone()), (json!("agent"), json!("sess-1")), "{rec}");
    let r = call("soup_reject", json!({"cauldron": "kb", "ids": [b[0]], "reason": "not useful"}));
    assert_eq!(answer_of(&r)["rejected"], json!([b[0]]), "{r}");
    assert_eq!(s.record(&b[0])["meta"]["rejected"]["reason"], "not useful");
    let r = call("soup_approve", json!({"cauldron": "kb", "ids": [a[1]], "undo": true}));
    assert_eq!(answer_of(&r)["unapproved"], json!([a[1]]), "{r}");
    let r = call("soup_approve", json!({"cauldron": "kb", "ids": [a[1]], "dump": "DM-1"}));
    assert_eq!(refusal_code(&r), "usage", "{r}");
    let audit = s.calls();
    for t in ["soup_approve", "soup_reject"] {
        assert!(audit.iter().any(|l| l["tool"] == t && l["client"] == "claude-code" && l["session"] == "sess-1" && l["by"] == "agent"), "{t}: {audit:?}");
    }
    // No record write sets a review flag, open or not.
    let mut t = task("t", "Approved by a write");
    t["meta"] = json!({"approved": {"at": "2026-01-01T00:00:00Z", "by": "tty"}});
    let r = svc.call(&s, "soup_put", json!({"cauldron": "kb", "remint": true, "records": [t]}));
    assert_eq!(r["isError"], json!(true), "{r}");
    // The ripples key never reaches a decision verb.
    let line = s.seam_line(&format!("ADECIDE {} {}", s.key("ripples.secret"), json!({"cauldron": "kb", "action": "approve", "ids": [a[1]]})));
    assert!(line.contains("\"unauthorized\""), "{line}");
    svc.stop();
}

/// **OA4b: the variable never opens the door.** A service whose own
/// environment carries `CAULDRON_OWNER_ACTS=approve` (started by hand; the
/// supervisor removes it) still refuses `soup_approve` and `soup_reject`
/// `owner_act_closed` while nothing is persisted, and `soup_status`,
/// `mcp status` and `owner-acts show` (run with the variable too) say
/// `terminal` for sessions while saying `env` for this command line. The
/// persisted `owner-acts open --only approve` opens it, and every view says
/// so for sessions.
#[test]
fn oa4b_the_variable_never_opens_the_door_and_the_views_say_so() {
    let s = Scratch::new("oa4b", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x")]);
    s.grant("kb");
    let svc = Service::start_bin_with(&s, &service_bin(), &[("CAULDRON_OWNER_ACTS", "approve")]);
    let pids = [svc.pid()];
    let call = |tool: &str, args: Value| svc.call_as(&s, tool, args, "claude-code", Some("sess-1"));
    for (tool, args) in [("soup_approve", json!({"cauldron": "kb", "ids": [a[0]]})), ("soup_reject", json!({"cauldron": "kb", "ids": [a[0]]}))] {
        let r = call(tool, args);
        assert_eq!(refusal_code(&r), "owner_act_closed", "{tool}: the service's own variable opened the door: {r}");
        assert!(r["content"][0]["text"].as_str().unwrap().contains("CAULDRON_OWNER_ACTS opens nothing here"), "{r}");
    }
    assert_eq!(s.queue(&[])["counts"]["unreviewed"], 1, "nothing written");
    let st = answer_of(&call("soup_status", json!({"cauldron": "kb"})));
    assert_eq!(st["owner_acts"]["approve"], "terminal", "{st}");
    let var = [("CAULDRON_OWNER_ACTS", "approve")];
    let m = s.run_with(&["mcp", "status", "--env", "dev"], &pids, &var).json();
    let row = m["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!((row["owner_acts"]["acts"]["approve"].clone(), row["owner_acts"]["sessions"]["approve"].clone()), (json!("env"), json!("terminal")), "{m}");
    assert_eq!(m["owner_acts"]["sessions"]["approve"], "terminal", "{m}");
    assert!(m["owner_acts"]["env_var"]["for"].as_str().unwrap().contains("never an MCP session"), "{m}");
    let v = s.run_with(&["owner-acts", "show", "--env", "dev"], &[], &var).json();
    let kb = v["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!((kb["acts"]["approve"].clone(), kb["sessions"]["approve"].clone()), (json!("env"), json!("terminal")), "{v}");
    // Persisted: open for sessions, and shown.
    assert_eq!(s.on_tty(&["owner-acts", "open", "--only", "approve"], &[], &[], None).rc, 0);
    let r = call("soup_approve", json!({"cauldron": "kb", "ids": [a[0]]}));
    assert_eq!((answer_of(&r)["by"].clone(), answer_of(&r)["approved"].clone()), (json!("agent"), json!([a[0]])), "{r}");
    let st = answer_of(&call("soup_status", json!({"cauldron": "kb"})));
    assert_eq!((st["owner_acts"]["approve"].clone(), st["owner_acts"]["from"].clone()), (json!("open"), json!("root")), "{st}");
    let m = s.run(&["mcp", "status", "--env", "dev"], &pids).json();
    let row = m["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!((row["owner_acts"]["acts"]["approve"].clone(), row["owner_acts"]["sessions"]["approve"].clone()), (json!("root"), json!("open")), "{m}");
    assert_eq!(m["owner_acts"]["sessions"]["approve"], "open", "{m}");
    let v = s.run(&["owner-acts", "show", "--env", "dev"], &[]).json();
    let kb = v["cauldrons"].as_array().unwrap().iter().find(|c| c["cauldron"] == "kb").unwrap().clone();
    assert_eq!(kb["sessions"]["approve"], "open", "{v}");
    svc.stop();
}

/// **OA5: across releases, no request is misread.** A newer command line
/// sends an agent's decision on a verb of its own (`ADECIDE`), so a service
/// that does not know it (here the fake of a release before it) answers
/// `ERR unknown request`, writes nothing, and the command names the
/// upgrade. The reverse: an older command line's `DECIDE` and `APPROVE`
/// (sent only after its terminal check) land on this service as `tty`, and
/// `ADECIDE` as `agent`. A root from the release before (no owner-acts
/// file) reads `terminal`, and a decision it recorded as `by: "owner"`
/// reads as `tty`.
#[test]
fn oa5_older_and_newer_releases_never_misread_an_owner_act() {
    // A newer command line, an older service.
    let s = Scratch::new("oa5", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x")]);
    s.grant("kb");
    // Persisted, so this command line gets as far as sending its ADECIDE.
    assert_eq!(s.run_with(&["owner-acts", "open", "--only", "approve"], &[], &[OPEN]).rc, 0);
    let heard = old_mcp(&s, env!("CARGO_PKG_VERSION"));
    let me = [std::process::id()];
    let before = s.queue(&[]);
    for args in [
        vec!["soup", "approve", a[0].as_str(), "--env", "dev", "--cauldron", "kb"],
        vec!["ripples", "reject", "--dump", "DM-1", "--env", "dev", "--cauldron", "kb"],
    ] {
        let o = s.run_with(&args, &me, &[OPEN]);
        assert_ne!(o.rc, 0, "{args:?}: {}", o.out);
        assert!(o.err.contains("does not know `ADECIDE`") && o.err.contains("It wrote nothing, and nothing was decided"), "{args:?}: {}", o.err);
    }
    let lines = heard.lock().unwrap().clone();
    assert!(!lines.is_empty() && lines.iter().all(|l| l.starts_with("ADECIDE ")), "only the new verb was sent: {lines:?}");
    assert_eq!(s.queue(&[])["counts"], before["counts"], "nothing was written");

    // A root from the release before, and an older command line.
    let s = Scratch::new("oa5b", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    let a = crate::ripples::ripple_write(&s.soup("kb"), "run-a", "DM-1", "sonnet", &[("idea", "one", "x"), ("idea", "two", "y"), ("idea", "three", "z"), ("idea", "four", "w")]);
    owner_decided(&s.soup("kb"), &a[3]);
    s.grant("kb");
    assert!(s.owner_acts_file().is_none());
    let v = s.run(&["owner-acts", "show", "--env", "dev"], &[]).json();
    assert!(v["cauldrons"].as_array().unwrap().iter().all(|c| c["setting"] == "terminal" && c["from"] == "default"), "{v}");
    let svc = Service::start(&s);
    assert_ne!(s.run(&["soup", "approve", &a[0], "--env", "dev", "--cauldron", "kb"], &[svc.pid()]).rc, 0, "upgraded root: terminal");
    let key = s.key("approve.secret");
    let line = s.seam_line(&format!("DECIDE {key} {}", json!({"cauldron": "kb", "action": "approve", "ids": [a[0]], "from": "cauldron ripples approve"})));
    assert!(line.contains("\"by\":\"tty\""), "{line}");
    let line = s.seam_line(&format!("APPROVE {key} {}", json!({"cauldron": "kb", "ids": [a[1]]})));
    assert!(line.contains("\"ok\":true"), "{line}");
    // An agent's claim is checked against the persisted setting: refused
    // under the upgraded root's `terminal`, taken once `approve` is open.
    let line = s.seam_line(&format!("ADECIDE {key} {}", json!({"cauldron": "kb", "action": "approve", "ids": [a[2]]})));
    assert!(line.contains("\"owner_act_closed\""), "an agent's claim under `terminal`: {line}");
    assert_eq!(s.run_with(&["owner-acts", "open", "--only", "approve"], &[], &[OPEN]).rc, 0);
    let line = s.seam_line(&format!("ADECIDE {key} {}", json!({"cauldron": "kb", "action": "approve", "ids": [a[2]]})));
    assert!(line.contains("\"by\":\"agent\""), "{line}");
    let line = s.seam_line(&format!("DECIDE {key} {}", json!({"cauldron": "kb", "action": "approve", "ids": [a[2]], "by": "agent"})));
    assert!(line.contains("\"usage\""), "a key DECIDE does not read is refused, never ignored: {line}");
    let by: Vec<Value> = a.iter().map(|id| s.record(id)["meta"]["approved"]["by"].clone()).collect();
    assert_eq!(by, vec![json!("tty"), json!("tty"), json!("agent"), json!("owner")]);
    let list = s.run(&["soup", "nodes", "--env", "dev", "--cauldron", "kb", "--review", "by-tty", "--limit", "100"], &[]).json();
    let mut ids: Vec<String> = list["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap().to_string()).collect();
    ids.sort();
    let mut want = vec![a[0].clone(), a[1].clone(), a[3].clone()];
    want.sort();
    assert_eq!(ids, want, "a decision recorded as the owner's reads as tty");
    svc.stop();
}

/// A decision as the release before this one wrote it (`by: "owner"`),
/// straight into the soup.
fn owner_decided(soup: &Path, id: &str) {
    use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
    let c = spine::Cauldron::open(soup, spine::config::SpineConfig::default()).unwrap();
    let mut bindings = std::collections::BTreeMap::new();
    bindings.insert("id".to_string(), json!(id));
    bindings.insert("v".to_string(), json!({"at": "2026-01-01T00:00:00Z", "by": "owner"}));
    let env = Envelope {
        statement: "MATCH (n: ws/idea { id: $id })\nSET n.meta.approved = $v".into(),
        bindings,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent { thread: "owner".into(), dispatch: "cauldron ripples approve".into() },
            instrument: Instrument { name: "test".into(), version: "0".into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!("owner-{id}")),
    };
    assert!(matches!(c.submit(env).unwrap(), spine::Outcome::Committed { .. }));
}

/// **A pinned bridge and the owner-act tools.** With `approve` opened to
/// agents for the whole root (so both cauldrons would take a decision), a
/// bridge pinned to `kb` refuses `soup_approve` and `soup_reject` naming
/// `other` as `cauldron_pinned`, before the service sees them: nothing is
/// decided in `other` and nothing is audited, whatever `$CAULDRON_CAULDRON`
/// says. The same tools naming no cauldron reach `kb` and decide as an
/// agent there.
#[test]
fn b6_a_pinned_bridge_refuses_the_owner_act_tools_for_another_cauldron() {
    let s = Scratch::new("b6", &["kb", "other"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    assert_eq!(s.import("other", &s.write("example2.json", &json!(example())), &[], &[]).rc, 0);
    let k = crate::ripples::ripple_write(&s.soup("kb"), "run-k", "DM-1", "sonnet", &[("idea", "kept", "x")]);
    let o = crate::ripples::ripple_write(&s.soup("other"), "run-o", "DM-1", "sonnet", &[("idea", "elsewhere", "y")]);
    s.grant("kb");
    soup_door::grants::grant(&s.env(), "other", &Scope::ALL, &[], "tty").unwrap();
    assert_eq!(s.on_tty(&["owner-acts", "open", "--only", "approve"], &[], &[], None).rc, 0);
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let mut b = Bridge::start_bare(&s, &s.0.join("work"), &["--env", "dev", "--cauldron", "kb"], &[("CAULDRON_CAULDRON", "other")], &pids);
    init(&mut b);
    let before = s.calls().len();
    for (n, (tool, args)) in [
        ("soup_approve", json!({"cauldron": "other", "ids": [o[0]]})),
        ("soup_approve", json!({"cauldron": " other ", "dump": "DM-1"})),
        ("soup_reject", json!({"cauldron": "other", "run": "run-o", "reason": "not here"})),
        ("soup_reject", json!({"cauldron": "other", "ids": [o[0]], "undo": true})),
    ]
    .into_iter()
    .enumerate()
    {
        let (code, msg) = refused(&b.call(20 + n as u64, tool, args));
        assert_eq!(code, "cauldron_pinned", "{tool}: {msg}");
        assert!(msg.contains("Nothing was read or written"), "{msg}");
    }
    assert_eq!(s.calls().len(), before, "nothing reached the service");
    let got = answer_of(&svc.call(&s, "soup_get", json!({"cauldron": "other", "ids": [o[0]]})));
    let meta = &got["records"][0]["meta"];
    assert!(meta.get("approved").is_none() && meta.get("rejected").is_none(), "`other` decided nothing: {got}");
    // Naming no cauldron, the pinned bridge reaches `kb`, where the owner
    // let agents decide.
    let r = b.call(30, "soup_approve", json!({"ids": [k[0]]}));
    let v = answer_of(&r);
    assert_eq!((v["by"].clone(), v["approved"].clone()), (json!("agent"), json!([k[0]])), "{r}");
    assert_eq!(s.record(&k[0])["meta"]["approved"]["by"], "agent");
    b.finish();
    svc.stop();
}

/// **`mcp config` opens nothing.** With every owner act opened (the
/// owner's recorded choice, and `CAULDRON_OWNER_ACTS=open` in the shell
/// that runs it), the printed `.mcp.json` entry is still only `command` and
/// `args`: no `env` block, and nothing that names the variable, on stdout
/// or stderr. The verb has no flag that would add one.
#[test]
fn b7_mcp_config_never_carries_owner_acts_into_a_project() {
    let s = Scratch::new("b7", &["kb"]);
    assert_eq!(s.import("kb", &s.write("example.json", &json!(example())), &[], &[]).rc, 0);
    s.grant("kb");
    let open = [("CAULDRON_OWNER_ACTS", "open")];
    assert_eq!(s.run_with(&["owner-acts", "open"], &[], &open).rc, 0);
    let o = s.run_with(&["mcp", "config", "--cauldron", "kb", "--env", "dev"], &[], &open);
    assert_eq!(o.rc, 0, "{}", o.err);
    let entry = o.json();
    let server = entry["mcpServers"]["cauldron-kb"].as_object().unwrap();
    let mut keys: Vec<&str> = server.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["args", "command"], "{entry}");
    assert!(!o.out.contains("OWNER_ACTS") && !o.err.contains("OWNER_ACTS"), "{}\n{}", o.out, o.err);
    let o = s.run_with(&["mcp", "config", "--cauldron", "kb", "--env", "dev", "--owner-acts", "open"], &[], &open);
    assert_ne!(o.rc, 0, "no flag carries it: {}", o.out);
    assert!(o.out.is_empty(), "{}", o.out);
}
