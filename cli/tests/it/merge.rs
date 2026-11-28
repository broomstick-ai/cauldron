//! **`cauldron merge <source> --into <target>`**: one cauldron's soup and
//! dumps copied into another, all or nothing.
//!
//! The fixture is two cauldrons of one environment, `a` (the source) and
//! `b` (the target), whose ids overlap (both have `R-1`, `R-2`, `CL-1`), a
//! region and a cluster of each with the same handle, a cluster of `a` that
//! claims two of its regions (edges across the two pools), dumps the two
//! share (the same bytes) and dumps only `a` has, a removed dump (a
//! tombstone), records a curator wrote and records a ripple wrote, and
//! (with `flags`) one record the owner approved and one rejected, through
//! the real `mcp` service at a terminal.
//!
//! - **M1** the plan: what a dry run prints, and that it writes nothing to
//!   either cauldron.
//! - **M2** a merge carrying review decisions needs a terminal (refused
//!   without one, nothing written); at one, everything lands: ids re-minted
//!   and printed, edges, claims, cites, `[[links]]` and boundaries rewritten,
//!   review state exactly as it was, dumps and blobs (shared bytes once),
//!   the tombstone; the source byte for byte as it was; the change feed's
//!   one change; the merge record; a second merge refused.
//! - **M3** the laundering attempts: no record write can claim to have
//!   come by a merge, and merging `a` into `b` and `b` back into `a` leaves
//!   every ripple record unreviewed and none approved (what came from `a` is
//!   already home and is not carried back).
//! - **M4** access: the source's grant does not open the target, the
//!   target's does not open the source, and the source's automatic setting
//!   is not carried.
//! - **M5** forwarded: the target held by the `mcp` service, the merge is
//!   sent to it (at a terminal) and lands through it; a source the service
//!   holds is refused.
//! - **M6** (`--features fault-injection`) the merge stopped after each of
//!   its steps: before its journal the target is unchanged (and recover
//!   sweeps what it staged); after it, every other writer is refused until
//!   `soup recover` abandons the merge (cut short before it held the
//!   target: nothing landed) or finishes it, and the target then holds
//!   exactly what an uninterrupted merge leaves. **M6b**: a merge recover
//!   cannot finish is put back from its snapshot, byte for byte.
//! - **M9** (fault injection, with `CAULDRON_PREVIOUS_CLI`) a 0.1.16
//!   `cauldron` importing and dumping after every pending step: refused
//!   wherever the merge holds a store; where it lands, recover still saves an
//!   id map naming exactly the merged records, and nothing is stuck.
//! - **M7** (with `CAULDRON_PREVIOUS_CLI` naming the 0.1.16 `cauldron`) the
//!   release before reads the merged target, and lists the merged records a
//!   ripple wrote that nobody decided as unreviewed, as this release does.
//! - **M10** a decision is carried only when the source's own write history
//!   vouches for it (the owner's decision, with who and when, or a merge
//!   that carried a vouched one); a forged flag, a flag changed by hand and
//!   a claimed provenance are not carried, and the plan counts both kinds
//!   first.
//! - **M11** a source already in the target by any route (merged before,
//!   or carried in through a third cauldron) is found by where each record
//!   came from and not carried again, listed as already here; a source that
//!   gained records since carries only those, pointing at the target's
//!   copies; nothing new is refused.
//! - **M12** ten thousand records merge as one write within a generous
//!   ceiling of time and memory.
//! - **M13** an edge from a record of the merge's main write to one of its
//!   ripple part lands after both, as the merge's third write.
//! - **M8** the snapshot is the whole target (both pools, the dumps store
//!   with every blob, a session's transcript included), beside it, private
//!   and verified; `cauldron merge --undo` puts the target back byte for
//!   byte, and refuses, changing nothing, once anything wrote to it since.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use soup_door::grants::Scope;

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");

/// `cauldron-mcp`, built from this worktree once per test process.
fn service_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let target = Path::new(CAULDRON).parent().unwrap().parent().unwrap();
        let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "cauldron-mcp", "--bin", "cauldron-mcp", "--message-format=json-render-diagnostics", "--target-dir"])
            .arg(target)
            // A fault build of this crate tests a fault build of the
            // service (its merge seam: `CAULDRON_MERGE_DELAY_MS`).
            .args(if cfg!(feature = "fault-injection") { &["--features", "fault-injection"][..] } else { &[][..] })
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stderr(Stdio::inherit())
            .output()
            .expect("spawn cargo build -p cauldron-mcp");
        assert!(out.status.success(), "building cauldron-mcp for the test: {:?}", out.status);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "cauldron-mcp")
            .find_map(|m| m["executable"].as_str().map(PathBuf::from))
            .expect("cargo reported no cauldron-mcp executable")
    })
    .clone()
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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

impl Scratch {
    fn new(tag: &str, cauldrons: &[&str]) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "cauldron-merge-it-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        for d in ["root/data/dev/mcp", "home", "work", "in"] {
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
    fn cauldron_dir(&self, c: &str) -> PathBuf {
        self.data().join("cauldrons").join(c)
    }
    fn soup(&self, c: &str) -> PathBuf {
        self.cauldron_dir(c).join("soup")
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
        soup_door::grants::grant(&self.env(), c, &Scope::ALL, &[], "tty").unwrap();
    }
    fn revoke(&self, c: &str) {
        soup_door::grants::revoke(&self.env(), c, None).unwrap();
    }
    fn command(&self, prog: &str, args: &[&str], pids: &[u32], vars: &[(&str, &str)]) -> Command {
        let mut c = Command::new(prog);
        c.args(args)
            .env("CAULDRON_HOME", self.root())
            .env("HOME", self.0.join("home"))
            .env(crate::grants::ALLOW_ENV, crate::grants::owned(&self.0, pids))
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .env_remove("CAULDRON_CAULDRON")
            .env_remove("CAULDRON_MERGE_FAULT")
            .envs(vars.iter().copied())
            .current_dir(self.0.join("work"));
        c
    }
    fn run_with(&self, prog: &str, args: &[&str], pids: &[u32], vars: &[(&str, &str)]) -> Out {
        let o = self.command(prog, args, pids, vars).stdin(Stdio::null()).output().expect("spawn");
        Out { rc: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into_owned(), err: String::from_utf8_lossy(&o.stderr).into_owned() }
    }
    fn run(&self, args: &[&str], pids: &[u32]) -> Out {
        self.run_with(CAULDRON, args, pids, &[])
    }
    /// The verb on a pseudo-terminal (`script -q /dev/null cauldron <args>`,
    /// the in-test way to give a command a terminal), answering a `[y/N]`
    /// question with `answer` when one is asked. Everything it prints
    /// arrives on stdout.
    fn on_tty(&self, args: &[&str], pids: &[u32], answer: Option<&str>) -> Out {
        let mut full = vec!["-q", "/dev/null", CAULDRON];
        full.extend_from_slice(args);
        let mut child = self.command("/usr/bin/script", &full, pids, &[]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
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
        if let Some(a) = answer {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !String::from_utf8_lossy(&out.lock().unwrap()).contains("[y/N]") && child.try_wait().unwrap().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = stdin.write_all(format!("{a}\n").as_bytes());
        }
        let status = child.wait().unwrap();
        drop(stdin);
        reader.join().unwrap();
        let text = String::from_utf8_lossy(&out.lock().unwrap()).replace("\r\n", "\n");
        Out { rc: status.code().unwrap_or(-1), out: text, err: String::new() }
    }
    fn file(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join("in").join(name);
        std::fs::write(&p, body).unwrap();
        p
    }
    fn import(&self, c: &str, records: &Value) -> Out {
        self.import_with(c, records, &[])
    }
    fn import_with(&self, c: &str, records: &Value, extra: &[&str]) -> Out {
        let f = self.file(&format!("{c}-{}.json", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()), &records.to_string());
        let mut a = vec!["soup", "import", f.to_str().unwrap(), "--env", "dev", "--cauldron", c];
        a.extend_from_slice(extra);
        self.run(&a, &[])
    }
    fn dump(&self, c: &str, text: &str) -> String {
        let f = self.file(&format!("dump-{c}-{}.txt", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()), text);
        let o = self.run(&["soup", "dump", f.to_str().unwrap(), "--env", "dev", "--cauldron", c], &[]);
        assert_eq!(o.rc, 0, "dump: {}{}", o.out, o.err);
        o.json()["dump_id"].as_str().unwrap().to_string()
    }
    fn get(&self, c: &str, id: &str) -> Value {
        let o = self.run(&["soup", "get", id, "--env", "dev", "--cauldron", c], &[]);
        assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
        o.json()["records"][0].clone()
    }
    fn queue(&self, c: &str) -> Value {
        let o = self.run(&["ripples", "review", "--env", "dev", "--cauldron", c], &[]);
        assert_eq!(o.rc, 0, "{}", o.err);
        o.json()
    }
    fn nodes(&self, c: &str) -> Vec<Value> {
        let o = self.run(&["soup", "nodes", "--env", "dev", "--cauldron", c, "--limit", "1000", "--include-rejected", "--include-superseded"], &[]);
        assert_eq!(o.rc, 0, "{}", o.err);
        o.json()["nodes"].as_array().unwrap().clone()
    }
    fn dumps(&self, c: &str) -> Vec<Value> {
        let o = self.run(&["soup", "dumps", "--env", "dev", "--cauldron", c, "--limit", "1000"], &[]);
        assert_eq!(o.rc, 0, "{}", o.err);
        o.json()["dumps"].as_array().unwrap().clone()
    }
    fn cursor(&self, c: &str) -> String {
        let o = self.run(&["soup", "changes", "--env", "dev", "--cauldron", c], &[]);
        o.json()["cursor"].as_str().unwrap().to_string()
    }
}

/// Every file under `dir`, by relative path, with its bytes.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.insert(p.strip_prefix(base).unwrap().display().to_string(), std::fs::read(&p).unwrap_or_default());
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

/// The JSON object a terminal run printed.
fn printed_json(text: &str) -> Value {
    let start = text.find("{\n").unwrap_or_else(|| panic!("no JSON in: {text}"));
    let end = text[start..].find("\n}").map(|e| start + e + 2).unwrap_or_else(|| panic!("no JSON end in: {text}"));
    serde_json::from_str(&text[start..end]).unwrap_or_else(|e| panic!("{e}: {text}"))
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
    fn start(s: &Scratch) -> Service {
        Service::start_bin(s, &service_bin())
    }
    /// The `mcp` service from `bin` (this worktree's, or an earlier
    /// release's).
    fn start_bin(s: &Scratch, bin: &Path) -> Service {
        Service::start_with(s, bin, &[])
    }
    /// [`Service::start_bin`] with `vars` set in its environment.
    fn start_with(s: &Scratch, bin: &Path, vars: &[(&str, &str)]) -> Service {
        let bin = bin.to_path_buf();
        let log = std::fs::File::create(s.0.join(format!("service-{}.log", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))).unwrap();
        let child = Command::new(&bin)
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
            .envs(vars.iter().copied())
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
    fn call(&self, s: &Scratch, tool: &str, mut args: Value) -> Value {
        args["_ctx"] = json!({"client": "test", "cwd": s.0.join("work").display().to_string()});
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).to_string();
        let secret = std::fs::read_to_string(s.mcp_dir().join("door.secret")).unwrap();
        let mut c = TcpStream::connect(self.door).unwrap();
        write!(
            c,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.door.port(),
            secret.trim(),
            body.len()
        )
        .unwrap();
        let mut raw = String::new();
        c.read_to_string(&mut raw).unwrap();
        let v: Value = serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap();
        v["result"].clone()
    }
    fn stop(mut self) {
        Command::new("/bin/kill").args(["-TERM", &self.child.id().to_string()]).status().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the service ignored SIGTERM");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

// ------------------------------------------------------------ the fixture

const SHARED: &str = "A note both cauldrons were given: the importer keeps edge text.";
const ONLY_A: &str = "A note only the source was given: clusters claim regions.";
const REMOVED: &str = "A note that was removed again.";
const ONLY_B: &str = "A note only the target was given.";

fn source_records() -> Value {
    json!([
        {"id": "R-1", "type": "region", "handle": "Importing records",
         "boundary": "Inside: anything about importing records into the soup. Outside: R-2 for everything else."},
        {"id": "R-2", "type": "region", "handle": "Everything about edges",
         "boundary": "Inside: what edges carry and how they are kept. Outside: R-1 for importing records.",
         "edges": [{"kind": "narrows", "to": "R-1"}]},
        {"id": "IS-1", "type": "issue", "state": "open", "handle": "The importer drops edge text", "body": "Settled by [[DEC-1]].",
         "meta": {"cites": [{"kind": "dump", "id": "DM-1"}]},
         "edges": [{"kind": "in_region", "to": "R-1", "note": "Outside: R-2 for everything else"}, {"kind": "ref", "to": "DEC-1"}]},
        {"id": "DEC-1", "type": "decision", "state": "decided", "handle": "Edges keep their text", "edges": [{"kind": "in_region", "to": "R-2"}]},
        {"id": "CL-1", "type": "cluster", "handle": "Import work",
         "boundary": "Inside: the importer and everything it does with records. Outside: nothing else.",
         "edges": [{"kind": "claims", "to": "R-1"}, {"kind": "claims", "to": "R-2"}]},
    ])
}

fn target_records() -> Value {
    json!([
        {"id": "R-1", "type": "region", "handle": "Importing records",
         "boundary": "Inside: how this project brings records in from elsewhere. Outside: R-2 for the rest."},
        {"id": "R-2", "type": "region", "handle": "The rest of the project",
         "boundary": "Inside: everything in this project not about bringing records in. Outside: R-1."},
        {"id": "T-1", "type": "task", "state": "ready", "handle": "Write the importer's tests", "edges": [{"kind": "in_region", "to": "R-1"}]},
        {"id": "CL-1", "type": "cluster", "handle": "Import work",
         "boundary": "Inside: the target's own work on bringing records in. Outside: nothing.",
         "edges": [{"kind": "claims", "to": "R-1"}]},
    ])
}

/// Ripple-written ids in `a`: `[C-1, C-2]` from run-a (citing DM-1) and
/// `[C-3, T-1]` from run-b (citing DM-2).
struct Fixture {
    s: Scratch,
    run_a: Vec<String>,
    run_b: Vec<String>,
}

/// `a` and `b` as the module docs say; with `flags`, the owner approved
/// `run_b[0]` and rejected `run_a[1]` at a terminal, through the service
/// (stopped again before this returns).
fn fixture(tag: &str, flags: bool) -> Fixture {
    let s = Scratch::new(tag, &["a", "b"]);
    for (c, r) in [("a", source_records()), ("b", target_records())] {
        let o = s.import(c, &r);
        assert_eq!(o.rc, 0, "import {c}: {}{}", o.out, o.err);
    }
    assert_eq!(s.dump("a", SHARED), "DM-1");
    assert_eq!(s.dump("a", ONLY_A), "DM-2");
    assert_eq!(s.dump("a", REMOVED), "DM-3");
    let o = s.run(&["soup", "dump-remove", "DM-3", "--env", "dev", "--cauldron", "a", "--yes"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(s.dump("b", SHARED), "DM-1");
    assert_eq!(s.dump("b", ONLY_B), "DM-2");
    let run_a = crate::ripples::ripple_write(&s.soup("a"), "run-a", "DM-1", "sonnet", &[("idea", "A ripple's first idea", "x"), ("idea", "A ripple's second idea", "y")]);
    let run_b = crate::ripples::ripple_write(&s.soup("a"), "run-b", "DM-2", "sonnet", &[("idea", "A ripple's third idea", "z"), ("task", "A ripple's task", "w")]);
    if flags {
        s.grant("a");
        let svc = Service::start(&s);
        let pids = [svc.pid()];
        let o = s.on_tty(&["ripples", "approve", &run_b[0], "--env", "dev", "--cauldron", "a"], &pids, None);
        assert_eq!(o.rc, 0, "{}", o.out);
        let o = s.on_tty(&["ripples", "reject", &run_a[1], "--env", "dev", "--cauldron", "a", "--reason", "not this"], &pids, None);
        assert_eq!(o.rc, 0, "{}", o.out);
        svc.stop();
    }
    Fixture { s, run_a, run_b }
}

fn merge(s: &Scratch, from: &str, into: &str, extra: &[&str]) -> Out {
    let mut a = vec!["merge", from, "--into", into, "--env", "dev"];
    a.extend_from_slice(extra);
    s.run(&a, &[])
}

// ------------------------------------------------------------------ M1

#[test]
fn m1_the_plan_is_printed_and_a_dry_run_writes_nothing() {
    let f = fixture("m1", true);
    let s = &f.s;
    let (a0, b0) = (files(&s.cauldron_dir("a")), files(&s.cauldron_dir("b")));
    let o = merge(s, "a", "b", &["--dry-run"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let p = o.json();
    assert_eq!((p["dry_run"].clone(), p["outcome"].clone(), p["merge"].clone()), (json!(true), json!("validated"), json!("MG-1")), "{p}");
    // Counts by type, in both pools; edges, and those across the pools.
    assert_eq!(p["counts"]["ws"], json!({"decision": 1, "idea": 3, "issue": 1, "region": 2, "task": 1}), "{p}");
    assert_eq!(p["counts"]["cauldron"], json!({"cluster": 1}));
    assert_eq!(p["counts"]["edges"]["claims"], json!(2));
    assert_eq!(p["counts"]["edges_across_pools"], json!(2));
    // The id collisions, and every source id with its new one.
    let coll: Vec<&str> = p["id_collisions"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    for id in ["R-1", "R-2", "CL-1", "T-1"] {
        assert!(coll.contains(&id), "{id} collides: {p}");
    }
    assert_eq!((p["id_map"]["R-1"].clone(), p["id_map"]["R-2"].clone(), p["id_map"]["CL-1"].clone()), (json!("R-3"), json!("R-4"), json!("CL-2")), "{p}");
    assert_eq!(p["id_map"].as_object().unwrap().len(), 9);
    // A region and a cluster with the target's handle: both kept, flagged.
    assert_eq!((p["flagged"]["regions"][0]["source"].clone(), p["flagged"]["regions"][0]["target"].clone()), (json!("R-1"), json!("R-1")), "{p}");
    assert_eq!(p["flagged"]["clusters"][0]["target"], json!("CL-1"));
    assert!(p["flagged"]["supersede"].as_str().unwrap().contains("supersedes"));
    // The regions with their boundaries.
    assert_eq!(p["regions"][1]["narrows"], json!(["R-3"]), "{p}");
    assert!(p["regions"][0]["boundary"].as_str().unwrap().starts_with("Inside:"));
    // Dumps: the shared bytes, the source's own, the tombstone.
    let d: BTreeMap<String, Value> = p["dumps"].as_array().unwrap().iter().map(|d| (d["id"].as_str().unwrap().to_string(), d.clone())).collect();
    assert!(d["DM-1"]["blob"].as_str().unwrap().starts_with("shared"), "{p}");
    assert_eq!(d["DM-2"]["blob"], json!("copied"));
    assert!(d["DM-3"]["blob"].as_str().unwrap().contains("tombstone"));
    assert_eq!((d["DM-1"]["new_id"].clone(), d["DM-3"]["new_id"].clone()), (json!("DM-3"), json!("DM-5")));
    // Review state, and the terminal it needs.
    assert_eq!(p["review"]["written_by_a_ripple"], json!({"unreviewed": 2, "approved": 1, "rejected": 1}), "{p}");
    assert_eq!(p["review"]["needs_terminal"], json!(true));
    // What is not carried, each with why.
    let nc: Vec<&str> = p["not_carried"].as_array().unwrap().iter().map(|n| n["what"].as_str().unwrap()).collect();
    for w in ["the source's MCP grant", "the source's automatic ripples setting", "the source itself"] {
        assert!(nc.contains(&w), "{w}: {p}");
    }
    assert_eq!(p["not_carried"][nc.iter().position(|w| *w == "the source's MCP grant").unwrap()]["source_has_one"], json!(true));
    assert_eq!(p["blocking"], json!([]));
    // Nothing was written, to either.
    assert!(files(&s.cauldron_dir("a")) == a0, "the dry run wrote to the source");
    assert!(files(&s.cauldron_dir("b")) == b0, "the dry run wrote to the target");
}

// ------------------------------------------------------------------ M2

#[test]
fn m2_a_merge_lands_whole_with_every_review_state_as_it_was() {
    let f = fixture("m2", true);
    let s = &f.s;
    let a_before = files(&s.cauldron_dir("a"));
    let b_before = files(&s.cauldron_dir("b"));
    let grants_before = std::fs::read(soup_door::grants::store_path(&s.env())).unwrap();
    let cursor = s.cursor("b");
    let a_approved = s.get("a", &f.run_b[0])["meta"]["approved"].clone();

    // No terminal: refused, naming the command, nothing written.
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    let j = o.json();
    assert_eq!(j["refusal"], json!("not_a_terminal"), "{j}");
    assert!(o.err.contains("the owner act `merge`") && o.err.contains("--only merge") && o.err.contains("cauldron merge a --into b --env dev"), "{}", o.err);
    assert_eq!(j["review"]["needs_terminal"], json!(true), "the plan comes with the refusal: {j}");
    assert!(files(&s.cauldron_dir("b")) == b_before, "a refused merge wrote to the target");

    // At a terminal: it lands.
    let o = s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let m = printed_json(&o.out);
    assert_eq!((m["outcome"].clone(), m["merge"].clone(), m["landed"]["records"].clone(), m["landed"]["dumps"].clone()), (json!("merged"), json!("MG-1"), json!(9), json!(3)), "{m}");
    let map = |id: &str| m["id_map"][id].as_str().unwrap().to_string();
    let dmap = |id: &str| m["dump_map"][id].as_str().unwrap().to_string();

    // Every record, with where it came from.
    assert_eq!(s.nodes("b").len(), 4 - 1 + 8, "b's 3 ws records plus a's 8 (clusters are listed apart)");
    let is1 = s.get("b", &map("IS-1"));
    assert_eq!(is1["body"], json!(format!("Settled by [[{}]].", map("DEC-1"))), "{is1}");
    assert_eq!(is1["meta"]["cites"][0]["id"], json!(dmap("DM-1")), "the cite names the dump's new id: {is1}");
    assert_eq!(is1["meta"]["merged_from"]["cauldron"], json!("a"));
    assert_eq!(is1["meta"]["merged_from"]["review"], json!("none"));
    assert_eq!(is1["merged"]["from"]["id"], json!("IS-1"), "{is1}");
    assert!(is1["edges"].as_array().unwrap().iter().any(|e| e["kind"] == "ref" && e["to"] == json!(map("DEC-1"))));
    assert!(is1["edges"][0]["note"].as_str().unwrap().contains(&map("R-2")), "an id a note names is rewritten: {is1}");
    let r1 = s.get("b", &map("R-1"));
    assert!(r1["boundary"].as_str().unwrap().contains(&format!("Outside: {} for", map("R-2"))), "{r1}");
    let r2 = s.get("b", &map("R-2"));
    assert_eq!(r2["edges"][0], json!({"kind": "narrows", "to": map("R-1")}));
    // The cluster and its claims across the pools.
    let cl = s.get("b", &map("CL-1"));
    let claimed: Vec<&str> = cl["claims"].as_array().unwrap().iter().map(|c| c["region"].as_str().unwrap()).collect();
    assert_eq!(claimed.len(), 2, "{cl}");
    assert!(claimed.contains(&map("R-1").as_str()) && claimed.contains(&map("R-2").as_str()), "{cl}");
    // The target's own records are where they were.
    assert_eq!(s.get("b", "T-1")["handle"], json!("Write the importer's tests"));
    assert_eq!(s.get("b", "R-1")["handle"], json!("Importing records"));

    // Review state exactly as it was.
    let q = s.queue("b");
    assert_eq!(q["counts"]["unreviewed"], json!(2), "{q}");
    let queued: Vec<String> = q["groups"].as_array().unwrap().iter().flat_map(|g| g["records"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string())).collect();
    let mut want = vec![map(&f.run_a[0]), map(&f.run_b[1])];
    want.sort();
    let mut got = queued.clone();
    got.sort();
    assert_eq!(got, want, "{q}");
    assert!(q["groups"].as_array().unwrap().iter().any(|g| g["run"] == "run-a" && g["dump"] == json!(dmap("DM-1"))), "{q}");
    let un = s.get("b", &map(&f.run_a[0]));
    assert_eq!((un["unreviewed"].clone(), un["meta"]["merged_from"]["review"].clone(), un["meta"]["merged_from"]["written_by"]["run"].clone()), (json!(true), json!("unreviewed"), json!("run-a")), "{un}");
    let ap = s.get("b", &map(&f.run_b[0]));
    assert!(ap.get("unreviewed").is_none(), "{ap}");
    assert_eq!((ap["meta"]["approved"]["at"].clone(), ap["meta"]["approved"]["by"].clone()), (a_approved["at"].clone(), json!("tty")), "the original decision: {ap}");
    assert_eq!(ap["meta"]["approved"]["carried_by_merge"]["id"], json!(f.run_b[0]), "{ap}");
    assert_eq!(ap["meta"]["approved"]["carried_by_merge"]["by"], json!("tty"), "who ran the merge: {ap}");
    let dec = &ap["meta"]["merged_from"]["decision"];
    assert_eq!((dec["by"].clone(), dec["how"].clone(), dec["thread"].clone()), (json!("tty"), json!("cauldron ripples approve"), json!("owner")), "who decided: {ap}");
    assert_eq!(dec["carried_by"], json!({"merge": "MG-1", "by": "tty"}), "{ap}");
    let rj = s.get("b", &map(&f.run_a[1]));
    assert_eq!((rj["rejected"].clone(), rj["meta"]["rejected"]["reason"].clone()), (json!(true), json!("not this")), "{rj}");
    assert!(rj["meta"]["rejected"]["carried_by_merge"].is_object());

    // Dumps: each carried in its state; the shared bytes held once.
    let ds = s.dumps("b");
    assert_eq!(ds.len(), 5, "{ds:?}");
    let state = |id: &str| ds.iter().find(|d| d["id"] == id).map(|d| d["state"].clone()).unwrap();
    assert_eq!((state(&dmap("DM-1")), state(&dmap("DM-3"))), (json!("received"), json!("removed")));
    let o = s.run(&["soup", "dump-get", &dmap("DM-2"), "--env", "dev", "--cauldron", "b", "--raw"], &[]);
    assert_eq!(o.out, ONLY_A, "{}", o.err);
    let blobs = std::fs::read_dir(s.cauldron_dir("b").join("dumps").join("blobs")).unwrap().count();
    assert_eq!(blobs, 3, "b's two blobs and the one only a held (the shared bytes once)");

    // The source, byte for byte as it was; the grants as they were.
    assert!(files(&s.cauldron_dir("a")) == a_before, "the merge wrote to the source");
    assert_eq!(std::fs::read(soup_door::grants::store_path(&s.env())).unwrap(), grants_before);

    // The change feed: the merge's writes, each saying it is the merge's:
    // the main one, and the ripple part (the records a ripple wrote that
    // nobody decided, in a write of the ripple class).
    let o = s.run(&["soup", "changes", "--env", "dev", "--cauldron", "b", "--since", &cursor], &[]);
    let ch = o.json();
    let creates: Vec<&Value> = ch["changes"].as_array().unwrap().iter().filter(|c| c.get("merge").is_some()).collect();
    assert_eq!(creates.len(), 2, "{ch}");
    assert_eq!((creates[0]["op"].clone(), creates[0]["count"].clone(), creates[0]["merge"]["id"].clone(), creates[0]["merge"]["part"].clone()), (json!("create"), json!(7), json!("MG-1"), json!("main")), "{ch}");
    assert_eq!(creates[0]["merge"]["counts"]["cluster"], json!(1));
    assert_eq!((creates[1]["count"].clone(), creates[1]["merge"]["part"].clone()), (json!(2), json!("ripple")), "{ch}");
    assert_eq!(ch["dumps"].as_array().unwrap().len(), 3, "each dump is its own write: {ch}");

    // The merge record and the saved map.
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(s.cauldron_dir("b").join("merges/MG-1/merge.json")).unwrap()).unwrap();
    let a_id = s.env().find_cauldron("a").unwrap().unwrap().id().unwrap();
    assert_eq!((rec["source"]["cauldron"].clone(), rec["source"]["cauldron_id"].clone(), rec["by"].clone()), (json!("a"), json!(a_id), json!("tty")), "{rec}");
    assert_eq!(rec["thread"], json!("owner"), "{rec}");
    assert_eq!(rec["id_map"], json!("merges/MG-1/id-map.json"));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(s.cauldron_dir("b").join("merges/MG-1/id-map.json")).unwrap()).unwrap();
    assert_eq!(saved["records"], m["id_map"]);
    let snap = PathBuf::from(rec["snapshot"]["dir"].as_str().unwrap());
    assert_eq!(snap.parent(), s.cauldron_dir("b").parent(), "the snapshot is beside the cauldron: {rec}");
    assert!(snap.join("files/soup/spine.db").is_file() && snap.join("files/dumps/blobs").is_dir(), "the pre-merge snapshot: {rec}");

    // A second merge of the same source carries nothing new: refused, at a
    // terminal too.
    let o = s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &[], None);
    assert_eq!(o.rc, 1, "{}", o.out);
    assert!(o.out.contains("already_merged") && o.out.contains("is already in `b`"), "{}", o.out);
}

// ------------------------------------------------------------------ M3

#[test]
fn m3_no_route_turns_an_unreviewed_record_into_an_approved_one() {
    let f = fixture("m3", false);
    let s = &f.s;
    // A record write cannot say it came by a merge: `soup import`, in any
    // casing and however deep ...
    for meta in [
        json!({"merged_from": {"written_by": {"class": "agent"}}}),
        json!({"Merged_From": {}}),
        json!({"cites": [{"kind": "url", "url": "https://example.org/a", "merged_from": {}}]}),
    ] {
        let o = s.import("b", &json!([{"id": "t", "type": "task", "state": "ready", "handle": "x", "meta": meta, "edges": [{"kind": "in_region", "to": "R-1"}]}]));
        assert_ne!(o.rc, 0, "{}", o.out);
        assert!(o.err.contains("written only by `cauldron merge`"), "{}", o.err);
    }
    // ... and `soup import-corpus`, whether the key rides on a node (carried
    // verbatim into `meta.imported_from.unmapped`) or in its cites.
    for node in [
        json!({"id": "N-1", "type": "task", "handle": "x", "state": "ready", "in_region": ["R-9"], "merged_from": {"id": "X-1"}}),
        json!({"id": "N-1", "type": "task", "handle": "x", "state": "ready", "in_region": ["R-9"], "cites": [{"kind": "url", "url": "https://example.org/b", "merged_from": {}}]}),
    ] {
        let corpus = json!({"schema_version": 5, "regions": [{"id": "R-9", "handle": "A region", "boundary": "Inside: everything here. Outside: nothing."}], "nodes": [node]});
        let f = s.file("corpus.json", &corpus.to_string());
        let o = s.run(&["soup", "import-corpus", f.to_str().unwrap(), "--env", "dev", "--cauldron", "b"], &[]);
        assert_ne!(o.rc, 0, "{}", o.out);
        assert!(o.err.contains("merged_from") && o.err.contains("written only by `cauldron merge`"), "{}", o.err);
    }
    // Nothing approved or rejected: no terminal needed.
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let m = o.json();
    assert_eq!(m["review"]["written_by_a_ripple"], json!({"unreviewed": 4, "approved": 0, "rejected": 0}));
    let qb = s.queue("b");
    assert_eq!((qb["counts"]["unreviewed"].clone(), qb["counts"]["approved"].clone()), (json!(4), json!(0)), "{qb}");
    // And back again: what came from `a` is already home, found by where
    // it came from, and is not carried a second time; only b's own records
    // come across, and nothing is approved.
    let o = merge(s, "b", "a", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let back = o.json();
    assert_eq!(back["review"]["written_by_a_ripple"], json!({"unreviewed": 0, "approved": 0, "rejected": 0}), "{back}");
    assert_eq!(back["already_here"]["records"].as_array().unwrap().len(), 9, "{back}");
    assert!(back["already_here"]["records"].as_array().unwrap().iter().all(|r| r["via"].as_str().unwrap().contains("the target's own record")), "{back}");
    assert_eq!(back["counts"]["ws"], json!({"region": 2, "task": 1}), "{back}");
    let qa = s.queue("a");
    assert_eq!((qa["counts"]["unreviewed"].clone(), qa["counts"]["approved"].clone()), (json!(4), json!(0)), "{qa}");
    for n in s.nodes("a") {
        let r = s.get("a", n["id"].as_str().unwrap());
        assert!(r["meta"].get("approved").is_none(), "{r}");
    }
    // A merge carrying no decision writes none.
    assert!(!s.nodes("b").iter().any(|n| s.get("b", n["id"].as_str().unwrap())["meta"].get("approved").is_some()));
}

// ------------------------------------------------------------------ M4

#[test]
fn m4_a_merge_never_opens_a_cauldron() {
    let f = fixture("m4", true);
    let s = &f.s;
    // a is granted (the fixture); turn its automatic processing on.
    let o = s.on_tty(&["ripples", "on", "a", "--env", "dev"], &[], Some("y"));
    assert_eq!(o.rc, 0, "{}", o.out);
    assert!(env::ripples_auto::load(&s.data(), "a").unwrap().unwrap().on);
    let o = s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    assert!(env::ripples_auto::load(&s.data(), "b").unwrap().is_none(), "the automatic setting was carried");
    let svc = Service::start(&s);
    // The source's grant does not open the target.
    let r = svc.call(s, "soup_status", json!({"cauldron": "b"}));
    assert_eq!(r["structuredContent"]["refused"]["code"], json!("not_granted"), "{r}");
    let r = svc.call(s, "soup_get", json!({"cauldron": "b", "ids": ["R-3"]}));
    assert_eq!(r["structuredContent"]["refused"]["code"], json!("not_granted"), "{r}");
    svc.stop();
    // The target's grant does not open the source.
    s.revoke("a");
    s.grant("b");
    let svc = Service::start(&s);
    let r = svc.call(s, "soup_status", json!({"cauldron": "a"}));
    assert_eq!(r["structuredContent"]["refused"]["code"], json!("not_granted"), "{r}");
    let r = svc.call(s, "soup_status", json!({"cauldron": "b"}));
    assert_eq!(r["isError"], json!(false), "{r}");
    svc.stop();
}

// ------------------------------------------------------------------ M5

#[test]
fn m5_a_target_the_service_holds_is_merged_through_it() {
    let f = fixture("m5", true);
    let s = &f.s;
    s.revoke("a");
    s.grant("b");
    let svc = Service::start(&s);
    let pids = [svc.pid()];
    let w = svc.call(s, "soup_put", json!({"cauldron": "b", "remint": true, "records": [{"id": "t", "type": "task", "state": "ready", "handle": "Held by the service", "edges": [{"kind": "in_region", "to": "R-1"}]}]}));
    assert_eq!(w["isError"], json!(false), "{w}");
    // Without a terminal: refused before anything is sent.
    let o = s.run(&["merge", "a", "--into", "b", "--env", "dev"], &pids);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("holds cauldron `b`") && o.err.contains("persisted owner_acts setting opens `merge`"), "{}", o.err);
    // At one: sent to the service, landed through it.
    let o = s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &pids, None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let m = printed_json(&o.out);
    assert_eq!((m["outcome"].clone(), m["forwarded_to"]["pid"].clone()), (json!("merged"), json!(svc.pid())), "{m}");
    let ap = svc.call(s, "soup_get", json!({"cauldron": "b", "ids": [m["id_map"][&f.run_b[0]].clone()]}));
    assert!(ap["structuredContent"]["records"][0]["meta"]["approved"]["carried_by_merge"].is_object(), "{ap}");
    let un = svc.call(s, "soup_get", json!({"cauldron": "b", "ids": [m["id_map"][&f.run_a[0]].clone()]}));
    assert_eq!(un["structuredContent"]["records"][0]["unreviewed"], json!(true), "{un}");
    let calls: Vec<Value> = std::fs::read_to_string(s.mcp_dir().join("calls.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(calls.iter().any(|l| l["tool"] == "merge" && l["outcome"] == "merged"), "{calls:?}");
    // A merged ripple record is decided in the target like any other: by
    // the owner, at a terminal.
    let merged = m["id_map"][&f.run_a[0]].as_str().unwrap().to_string();
    let o = s.on_tty(&["ripples", "approve", &merged, "--env", "dev", "--cauldron", "b"], &pids, None);
    assert_eq!(printed_json(&o.out)["approved"], json!([merged]), "{}", o.out);
    assert_eq!(s.queue("b")["counts"]["unreviewed"], json!(1));
    // The service still writes to b afterwards.
    let w = svc.call(s, "soup_put", json!({"cauldron": "b", "remint": true, "records": [{"id": "t", "type": "task", "state": "ready", "handle": "After the merge", "edges": [{"kind": "in_region", "to": "R-1"}]}]}));
    assert_eq!(w["isError"], json!(false), "{w}");
    // A source the service holds is refused, and nothing is written.
    let o = s.run(&["create", "c", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0);
    // The target's writer lock is taken (and so made) before the source is
    // found held; nothing else is.
    let unlocked = |d: &Path| -> BTreeMap<String, Vec<u8>> { files(d).into_iter().filter(|(k, _)| !k.ends_with("writer.lock")).collect() };
    let c_before = unlocked(&s.cauldron_dir("c"));
    let o = s.on_tty(&["merge", "b", "--into", "c", "--env", "dev"], &pids, None);
    assert_eq!(o.rc, 1, "{}", o.out);
    assert!(o.out.contains("the source, cauldron `b`, is held") && o.out.contains("`mcp` service"), "{}", o.out);
    assert!(unlocked(&s.cauldron_dir("c")) == c_before, "a refused merge wrote to the target");
    svc.stop();
    // Directly, once nothing holds b.
    let o = s.on_tty(&["merge", "b", "--into", "c", "--env", "dev"], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
}

// ------------------------------------------------------------------ M8

/// A session dump in `c`, written as the dump tools write one: its
/// transcript lives only in its blob.
fn session_dump(s: &Scratch, c: &str, text: &str) -> String {
    use std::io::Write as _;
    let cauldron = s.env().find_cauldron(c).unwrap().unwrap();
    let st = soup_door::dumps::Store::open(&cauldron, "test").unwrap();
    let mut w = st.blob(soup_door::dumps::Kind::Session).unwrap();
    w.write_all(text.as_bytes()).unwrap();
    let by = soup_door::dumps::By { thread: "test".into(), dispatch: "test session".into(), instrument: "test".into(), version: "0".into() };
    let (_, landed) = st
        .commit(
            w,
            || Ok::<(), soup_door::Error>(()),
            |b| {
                Ok(soup_door::dumps::NewDump {
                    env: "dev".into(),
                    content: soup_door::dumps::Content::Session { session_id: "the-target-s-own-session".into(), cwd: Some("/w".into()), lines: 1 },
                    blob: b.clone(),
                    title: None,
                    cite: None,
                    refs: vec![],
                    client: "test".into(),
                    session: None,
                })
            },
            &by,
            |e| e,
        )
        .unwrap();
    landed.id
}

/// Every file of a cauldron's directory but its writer locks, with its bytes.
fn unlocked(d: &Path) -> BTreeMap<String, Vec<u8>> {
    files(d).into_iter().filter(|(k, _)| !k.ends_with("writer.lock")).collect()
}

#[test]
fn m8_undo_puts_the_target_back_exactly_and_refuses_after_a_later_write() {
    let f = fixture("m8", false);
    let s = &f.s;
    let transcript = "{\"type\":\"user\",\"cwd\":\"/w\",\"message\":{\"content\":\"the target's own session\"}}\n";
    let sd = session_dump(s, "b", transcript);
    let raw = |c: &str, id: &str| s.run(&["soup", "dump-get", id, "--env", "dev", "--cauldron", c, "--raw"], &[]);
    assert_eq!(raw("b", &sd).out, transcript);
    let before = unlocked(&s.cauldron_dir("b"));

    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let m = o.json();
    let snap = PathBuf::from(m["snapshot"]["dir"].as_str().unwrap());
    // Beside the cauldron, dated, private, complete and verified.
    let name = snap.file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with("b.MG-1-before-") && snap.parent() == s.cauldron_dir("b").parent(), "{name}");
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(snap.join("manifest.json")).unwrap()).unwrap();
    let listed: Vec<&str> = manifest["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    for want in ["soup/spine.db", "soup/ws.db", "dumps/spine.db", "dumps/dumps.db"] {
        assert!(listed.contains(&want), "{want}: {listed:?}");
    }
    let blobs_before = before.keys().filter(|k| k.starts_with("dumps/blobs/")).count();
    assert_eq!(listed.iter().filter(|k| k.starts_with("dumps/blobs/")).count(), blobs_before, "every blob, the transcript's too");
    assert_eq!(manifest["count"]["files"], json!(before.len()), "every file but the writer locks: {manifest}");
    assert!(manifest["files"].as_array().unwrap().iter().all(|f| f["sha256"].as_str().is_some_and(|h| h.len() == 64)));
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&snap), 0o700);
        assert_eq!(mode(&snap.join("files/dumps/dumps.db")), 0o600);
        assert_eq!(mode(&snap.join("manifest.json")), 0o600);
    }
    assert_eq!(m["undo"], json!("cauldron merge --undo MG-1 --into b --env dev"));
    assert_ne!(unlocked(&s.cauldron_dir("b")), before);

    // Undone: the whole directory, byte for byte, the transcript included.
    let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(o.json()["outcome"], json!("undone"));
    let after = unlocked(&s.cauldron_dir("b"));
    let differ: Vec<&String> = before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).collect();
    assert!(differ.is_empty(), "after the undo these differ from before the merge: {differ:?}");
    assert_eq!(raw("b", &sd).out, transcript, "the target's own transcript");
    // Nothing was deleted: what the merge added is kept in the snapshot.
    let undone: Value = serde_json::from_str(&std::fs::read_to_string(snap.join("undo.json")).unwrap()).unwrap();
    assert!(Path::new(undone["restored"]["aside"].as_str().unwrap()).join("merges/MG-1/merge.json").is_file(), "{undone}");
    // Undoing it again: there is no such merge any more.
    let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
    assert_eq!((o.rc, o.json()["refusal"].clone()), (1, json!("not_found")), "{}", o.out);

    // Merged again, then written to: the undo is refused, says what changed,
    // and changes nothing.
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let id = o.json()["merge"].as_str().unwrap().to_string();
    let o = s.import_with("b", &json!([{"id": "t", "type": "task", "state": "ready", "handle": "Written after the merge", "edges": [{"kind": "in_region", "to": "R-1"}]}]), &["--remint"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let written = unlocked(&s.cauldron_dir("b"));
    let o = s.run(&["merge", "--undo", &id, "--into", "b", "--env", "dev"], &[]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    let j = o.json();
    assert_eq!(j["refusal"], json!("changed_since"), "{j}");
    assert!(o.err.contains("soup write(s) that are not the merge's: 1 added") && o.err.contains("record(s) that are not the merge's: 1 added") && o.err.contains("supersede"), "{}", o.err);
    assert_eq!(j["changed_outside_the_merge"]["records"]["added"], json!(1), "{j}");
    assert_eq!(unlocked(&s.cauldron_dir("b")), written, "a refused undo changed the target");
}

// ------------------------------------------------------------------ M10

/// Set `meta.<flag>` of record `id` straight in a cauldron's pool file, as
/// an edit of the user's own files would: no transaction records it.
fn forge(s: &Scratch, c: &str, id: &str, path: &str, value: Value) {
    let db = rusqlite::Connection::open(s.soup(c).join("ws.db")).unwrap();
    let n = db
        .execute(
            "UPDATE node_fields SET value = json_set(value, ?1, json(?2)) WHERE node_id = ?3 AND field = 'meta'",
            rusqlite::params![path, value.to_string(), id],
        )
        .unwrap();
    assert_eq!(n, 1, "forging {path} on {id}");
}

#[test]
fn m10_a_decision_is_carried_only_when_the_source_s_history_vouches_for_it() {
    let f = fixture("m10", true);
    let s = &f.s;
    // The owner's own decisions (the fixture: run_b[0] approved, run_a[1]
    // rejected, at a terminal) are vouched for. A forged approval on
    // run_a[0], and an approval changed by hand after the owner's, are not;
    // nor is a provenance a record claims that no merge gave it.
    forge(s, "a", &f.run_a[0], "$.approved", json!({"at": "2026-01-01T00:00:00Z", "by": "owner"}));
    forge(s, "a", &f.run_b[0], "$.approved.by", json!("someone else"));
    forge(s, "a", &f.run_b[1], "$.merged_from", json!({"merge": "MG-9", "cauldron_id": "elsewhere", "id": "X-1", "written_by": {"class": "agent"}}));
    let o = merge(s, "a", "b", &["--dry-run"]);
    let p = o.json();
    let d = &p["review"]["decisions"];
    assert_eq!((d["will_be_carried"].clone(), d["will_not_be_carried"].clone()), (json!(1), json!(2)), "the plan counts them before anything is written: {d}");
    assert_eq!(d["carried"][0]["id"], json!(f.run_a[1]), "{d}");
    assert_eq!((d["carried"][0]["flag"].clone(), d["carried"][0]["by"].clone()), (json!("rejected"), json!("tty")), "{d}");
    let why: BTreeMap<String, String> = d["not_carried"].as_array().unwrap().iter().map(|x| (x["id"].as_str().unwrap().to_string(), x["why"].as_str().unwrap().to_string())).collect();
    assert!(why[&f.run_a[0]].contains("holds no write of `meta.approved`"), "{why:?}");
    assert!(why[&f.run_b[0]].contains("wrote another value"), "{why:?}");
    assert!(p["not_carried"].to_string().contains("claimed merge provenance") && p["not_carried"].to_string().contains(&f.run_b[1]), "{p}");
    assert_eq!(p["review"]["written_by_a_ripple"], json!({"unreviewed": 3, "approved": 0, "rejected": 1}), "{p}");
    // It lands (the one vouched rejection needs the terminal).
    let o = s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let m = printed_json(&o.out);
    let map = |id: &str| m["id_map"][id].as_str().unwrap().to_string();
    for id in [&f.run_a[0], &f.run_b[0]] {
        let r = s.get("b", &map(id));
        assert_eq!(r["unreviewed"], json!(true), "an unvouched decision is not carried: {r}");
        assert!(r["meta"].get("approved").is_none(), "{r}");
        let n = &r["meta"]["merged_from"]["decision_not_carried"];
        assert_eq!(n["claimed"], json!("approved"), "{r}");
        assert!(n["why"].as_str().unwrap().len() > 20, "{r}");
    }
    let rj = s.get("b", &map(&f.run_a[1]));
    assert_eq!(rj["rejected"], json!(true), "{rj}");
    let dec = &rj["meta"]["merged_from"]["decision"];
    assert_eq!((dec["by"].clone(), dec["how"].clone()), (json!("tty"), json!("cauldron ripples reject")), "who and when: {rj}");
    assert!(dec["tx"].as_str().is_some_and(|t| t.len() == 64) && dec["at"].is_string(), "{rj}");
    let claimed = s.get("b", &map(&f.run_b[1]));
    assert_ne!(claimed["meta"]["merged_from"]["cauldron_id"], json!("elsewhere"), "{claimed}");
    assert!(claimed["meta"]["merged_from"].get("via").is_none(), "{claimed}");
    assert_eq!(s.queue("b")["counts"]["unreviewed"], json!(3));

    // Carried on: a merge of b into a fresh cauldron carries the vouched
    // rejection again (its source's merge recorded it), and nothing else.
    let o = s.run(&["create", "c", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let p = merge(s, "b", "c", &["--dry-run"]).json();
    let d = &p["review"]["decisions"];
    assert_eq!((d["will_be_carried"].clone(), d["will_not_be_carried"].clone()), (json!(1), json!(0)), "{d}");
    assert!(d["carried"][0]["by"] == json!("tty"), "{d}");
}

/// One decision written into `c`'s soup as `thread` running `dispatch`
/// would write it (the same SET the `mcp` service makes), with the flag's
/// `value`.
fn decide_as(s: &Scratch, c: &str, id: &str, flag: &str, value: Value, thread: &str, dispatch: &str) {
    use spine::envelope::{Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION};
    let h = spine::Cauldron::open(&s.soup(c), spine::config::SpineConfig::default()).unwrap();
    let ty = s.get(c, id)["type"].as_str().unwrap().to_string();
    let env = Envelope {
        statement: format!("MATCH (n: ws/{ty} {{ id: $id }})\nEXPECT COUNT(n) = 1\nSET n.meta.{flag} = $v"),
        bindings: [("id".to_string(), json!(id)), ("v".to_string(), value)].into_iter().collect(),
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent { thread: thread.into(), dispatch: dispatch.into() },
            instrument: Instrument { name: "test".into(), version: "0".into() },
        },
        mode: Mode::Commit,
        idempotency_nonce: Some(format!("decide-{id}-{thread}")),
    };
    assert!(matches!(h.submit(env).unwrap(), spine::Outcome::Committed { .. }));
}

// ------------------------------------------------------------------ M14

#[test]
fn m14_a_decision_an_agent_made_where_the_owner_allowed_it_is_carried_as_one_and_merge_is_an_owner_act() {
    let f = fixture("m14", false);
    let s = &f.s;
    // The owner lets agents approve for `a`; an agent's shell approves one
    // record (ADECIDE, through the service) and a session rejects another
    // through the door's tool (recorded as the service records it).
    let o = s.run_with(CAULDRON, &["owner-acts", "open", "--only", "approve", "--cauldron", "a", "--env", "dev"], &[], &[("CAULDRON_OWNER_ACTS", "approve")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    s.grant("a");
    let svc = Service::start(s);
    let o = s.run_with(CAULDRON, &["ripples", "approve", &f.run_b[0], "--env", "dev", "--cauldron", "a"], &[svc.pid()], &[]);
    assert_eq!(o.rc, 0, "an agent's approval where the owner opened it: {}{}", o.out, o.err);
    svc.stop();
    s.revoke("a");
    let at = "2026-11-20T00:00:00Z";
    decide_as(s, "a", &f.run_a[1], "rejected", json!({"at": at, "by": "agent", "session": "s-1", "reason": "off topic"}), "mcp:claude-code:s-1", "mcp soup_reject");
    // An agent-class write that is no decision command is not one.
    decide_as(s, "a", &f.run_a[0], "approved", json!({"at": at, "by": "agent"}), "owner:agent", "a script");
    let p = merge(s, "a", "b", &["--dry-run"]).json();
    let d = &p["review"]["decisions"];
    assert_eq!((d["will_be_carried"].clone(), d["will_not_be_carried"].clone()), (json!(2), json!(1)), "{d}");
    let by: BTreeMap<String, String> = d["carried"].as_array().unwrap().iter().map(|x| (x["id"].as_str().unwrap().to_string(), x["by"].as_str().unwrap().to_string())).collect();
    assert_eq!((by[&f.run_b[0]].as_str(), by[&f.run_a[1]].as_str()), ("agent", "agent"), "{d}");
    assert!(d["not_carried"][0]["why"].as_str().unwrap().contains("not a decision"), "{d}");
    assert_eq!(p["review"]["needs_terminal"], json!(true), "{p}");

    // Carrying them is the owner act `merge`: refused without a terminal
    // while it is closed (approve being open does not open it) ...
    let b_before = files(&s.cauldron_dir("b"));
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    assert_eq!(o.json()["refusal"], json!("not_a_terminal"));
    assert!(o.err.contains("cauldron owner-acts open --only merge") && o.err.contains("CAULDRON_OWNER_ACTS=merge"), "{}", o.err);
    assert!(files(&s.cauldron_dir("b")) == b_before, "a refused merge wrote to the target");
    // ... and lands where the variable opens it for this command, recorded
    // as an agent's merge on the record and on each carried decision.
    let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_OWNER_ACTS", "merge")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let m = o.json();
    let map = |id: &str| m["id_map"][id].as_str().unwrap().to_string();
    let ap = s.get("b", &map(&f.run_b[0]));
    let dec = &ap["meta"]["merged_from"]["decision"];
    assert_eq!((dec["by"].clone(), dec["thread"].clone(), dec["how"].clone()), (json!("agent"), json!("owner:agent"), json!("cauldron ripples approve")), "{ap}");
    assert_eq!(dec["carried_by"], json!({"merge": "MG-1", "by": "agent"}), "{ap}");
    assert_eq!(ap["meta"]["approved"]["carried_by_merge"]["by"], json!("agent"), "{ap}");
    assert!(ap.get("unreviewed").is_none(), "{ap}");
    let rj = s.get("b", &map(&f.run_a[1]));
    let dec = &rj["meta"]["merged_from"]["decision"];
    assert_eq!((rj["rejected"].clone(), dec["by"].clone(), dec["session"].clone(), dec["how"].clone()), (json!(true), json!("agent"), json!("s-1"), json!("mcp soup_reject")), "{rj}");
    let un = s.get("b", &map(&f.run_a[0]));
    assert_eq!(un["unreviewed"], json!(true), "{un}");
    // A ripple's record whose decision was carried landed in the merge's
    // main write, and its `dump` citation is still its review trail: not
    // removable, as on any ripple's record.
    let o = s.run(&["soup", "cite", &map(&f.run_b[0]), "--remove", "0", "--env", "dev", "--cauldron", "b"], &[]);
    assert_ne!(o.rc, 0, "{}{}", o.out, o.err);
    assert!(format!("{}{}", o.out, o.err).contains("dump-cite-is-provenance"), "{}{}", o.out, o.err);
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(s.cauldron_dir("b").join("merges/MG-1/merge.json")).unwrap()).unwrap();
    assert_eq!((rec["by"].clone(), rec["thread"].clone()), (json!("agent"), json!("owner:agent")), "{rec}");
    assert!(rec["allowed"].as_str().unwrap().contains("CAULDRON_OWNER_ACTS"), "{rec}");

    // Through the service: an agent's merge goes as AMERGE, refused until
    // the owner's PERSISTED setting opens `merge` (the variable never
    // reaches the service), and then recorded as an agent's.
    let o = s.run(&["create", "c", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(s.import("c", &target_records()).rc, 0);
    s.grant("c");
    let svc = Service::start(s);
    let pids = [svc.pid()];
    let w = svc.call(s, "soup_put", json!({"cauldron": "c", "remint": true, "records": [{"id": "t", "type": "task", "state": "ready", "handle": "Held by the service", "edges": [{"kind": "in_region", "to": "R-1"}]}]}));
    assert_eq!(w["isError"], json!(false), "{w}");
    let o = s.run_with(CAULDRON, &["merge", "a", "--into", "c", "--env", "dev"], &pids, &[("CAULDRON_OWNER_ACTS", "merge")]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    assert!(o.err.contains("persisted owner_acts setting opens `merge`") && o.err.contains("never reaches the service"), "{}", o.err);
    let o = s.run_with(CAULDRON, &["owner-acts", "open", "--only", "merge", "--cauldron", "c", "--env", "dev"], &[], &[("CAULDRON_OWNER_ACTS", "merge")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let o = s.run(&["merge", "a", "--into", "c", "--env", "dev"], &pids);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let m = o.json();
    assert_eq!(m["forwarded_to"]["service"], json!("mcp"), "{m}");
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(s.cauldron_dir("c").join("merges/MG-1/merge.json")).unwrap()).unwrap();
    assert_eq!(rec["by"], json!("agent"), "{rec}");
    svc.stop();
}

// ------------------------------------------------------------------ M11

#[test]
fn m11_a_source_already_here_by_any_route_is_not_carried_twice_and_only_what_is_new_is() {
    let f = fixture("m11", false);
    let s = &f.s;
    let o = s.run(&["create", "c", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    // a into b, b into c: c holds a's records by a second route.
    assert_eq!(merge(s, "a", "b", &[]).rc, 0);
    let o = merge(s, "b", "c", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let unreviewed = s.queue("c")["counts"]["unreviewed"].clone();
    assert_eq!(unreviewed, json!(4));
    // a into c: everything a holds is already in c, found by its origin.
    let o = merge(s, "a", "c", &["--dry-run"]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    let p = o.json();
    assert_eq!(p["outcome"], json!("blocked"), "{p}");
    assert!(p["blocking"][0]["why"].as_str().unwrap().contains("is already in `c`"), "{p}");
    let here = p["already_here"]["records"].as_array().unwrap();
    assert_eq!(here.len(), 9, "{p}");
    assert!(here.iter().all(|r| r["via"] == json!("already here via MG-1")), "{p}");
    assert_eq!(p["already_here"]["dumps"].as_array().unwrap().len(), 3, "{p}");
    let o = merge(s, "a", "c", &[]);
    assert_eq!((o.rc, o.json()["refusal"].clone()), (1, json!("already_merged")), "{}", o.out);
    assert_eq!(s.queue("c")["counts"]["unreviewed"], unreviewed, "nothing was carried twice");

    // a gains a record (in a region already in b, citing a dump already in b,
    // superseding a record already in b) and a dump: merged again, only those
    // two come across, pointing at b's copies.
    let new = json!([{"id": "n", "type": "decision", "state": "decided", "handle": "A decision made after the first merge",
                      "body": "Replaces [[DEC-1]].", "meta": {"cites": [{"kind": "dump", "id": "DM-2"}]},
                      "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "supersedes", "to": "DEC-1"}]}]);
    let o = s.import_with("a", &new, &["--remint"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let added = o.json()["id_map"]["n"].as_str().unwrap().to_string();
    let late = s.dump("a", "A note given to the source after the first merge.");
    let first: Value = serde_json::from_str(&std::fs::read_to_string(s.cauldron_dir("b").join("merges/MG-1/id-map.json")).unwrap()).unwrap();
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let m = o.json();
    assert_eq!(m["landed"]["records"], json!(1), "{m}");
    assert_eq!(m["landed"]["dumps"], json!(1), "{m}");
    assert_eq!(m["already_here"]["records"].as_array().unwrap().len(), 9, "{m}");
    assert!(m["notes"].to_string().contains("not carried again"), "{m}");
    let got = s.get("b", m["id_map"][&added].as_str().unwrap());
    let r1 = first["records"]["R-1"].as_str().unwrap();
    let dec1 = first["records"]["DEC-1"].as_str().unwrap();
    assert!(got["edges"].as_array().unwrap().iter().any(|e| e["kind"] == "in_region" && e["to"] == json!(r1)), "{got}");
    assert!(got["edges"].as_array().unwrap().iter().any(|e| e["kind"] == "supersedes" && e["to"] == json!(dec1)), "{got}");
    assert_eq!(got["body"], json!(format!("Replaces [[{dec1}]].")), "{got}");
    assert_eq!(got["meta"]["cites"][0]["id"], first["dumps"]["DM-2"], "the cite names b's copy of the dump: {got}");
    assert_eq!(m["dump_map"].as_object().unwrap().keys().collect::<Vec<_>>(), vec![&late], "{m}");
    // And again: nothing new.
    let o = merge(s, "a", "b", &[]);
    assert_eq!((o.rc, o.json()["refusal"].clone()), (1, json!("already_merged")), "{}", o.out);
}

// ------------------------------------------------------------------ M12

/// **Scale**: a source of 10,000 records (and 500 a ripple wrote, and 20
/// dumps) merges as one write within a generous ceiling of time and memory,
/// measured on the `cauldron` process itself. Release builds take a few
/// seconds and under 400 MB here; this test runs the debug build, so its
/// ceilings (120 s, 1.5 GB of resident memory) are wide enough for a busy
/// machine and still catch a return of the per-record judging (minutes)
/// or of a lookup that grows with the square of the batch.
#[test]
fn m12_ten_thousand_records_merge_within_a_generous_ceiling() {
    let s = Scratch::new("m12", &["a", "b"]);
    let regions = json!([
        {"id": "R-1", "type": "region", "handle": "Importing records",
         "boundary": "Inside: anything about importing records into the soup. Outside: R-2 for everything else."},
        {"id": "R-2", "type": "region", "handle": "Everything about edges",
         "boundary": "Inside: what edges carry and how they are kept. Outside: R-1 for importing records.", "edges": [{"kind": "narrows", "to": "R-1"}]},
    ]);
    let mut recs = regions.as_array().unwrap().clone();
    for i in 1..=10_000u32 {
        let mut r = json!({"id": format!("C-{i}"), "type": "idea", "state": "idea", "handle": format!("Idea number {i} about records"),
                           "body": format!("see [[C-{}]]", i.saturating_sub(1).max(1)), "edges": [{"kind": "in_region", "to": format!("R-{}", 1 + i % 2)}]});
        if i > 1 && i % 3 == 0 {
            r["edges"].as_array_mut().unwrap().push(json!({"kind": "ref", "to": format!("C-{}", i - 1)}));
        }
        recs.push(r);
    }
    let o = s.import("a", &Value::Array(recs));
    assert_eq!(o.rc, 0, "{}{}", &o.out[..o.out.len().min(3000)], o.err);
    assert_eq!(s.import("b", &regions).rc, 0);
    for i in 0..20 {
        s.dump("a", &format!("dump number {i} {}", "x".repeat(2000)));
    }
    let many: Vec<(String, String, String)> = (0..500).map(|i| ("idea".to_string(), format!("A ripple's idea {i}"), "b".to_string())).collect();
    let refs: Vec<(&str, &str, &str)> = many.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
    crate::ripples::ripple_write(&s.soup("a"), "run-big", "DM-1", "sonnet", &refs);
    let t0 = Instant::now();
    let o = s.run_with("/usr/bin/time", &["-l", CAULDRON, "merge", "a", "--into", "b", "--env", "dev"], &[], &[]);
    let took = t0.elapsed();
    assert_eq!(o.rc, 0, "{}", &o.err[o.err.len().saturating_sub(2000)..]);
    let rss: u64 = o.err.lines().find(|l| l.contains("maximum resident set size")).and_then(|l| l.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap();
    let m = o.json();
    println!("m12: merged {} records and {} dumps in {:.1} s, {} MB resident at most", m["landed"]["records"], m["landed"]["dumps"], took.as_secs_f64(), rss >> 20);
    assert_eq!((m["landed"]["records"].clone(), m["landed"]["dumps"].clone()), (json!(10_502), json!(20)), "{}", m["landed"]);
    assert!(took < Duration::from_secs(120), "the merge took {took:?}");
    assert!(rss < 1536 << 20, "the merge held {} MB", rss >> 20);
    assert_eq!(s.queue("b")["counts"]["unreviewed"], json!(500));
}

// ------------------------------------------------------------------ M17

/// **A source over the ceiling is refused before anything is written.** A
/// source of 20,001 records: the dry run and the merge are each refused
/// (`too_large`), with the size and the estimate, naming `--allow-large`,
/// and the target (every file, its writer lock included) and the source
/// are byte for byte as they were. With `--allow-large` the dry run goes on
/// and says the estimate and that the target refuses writes meanwhile.
#[test]
fn m17_a_source_over_the_ceiling_is_refused_before_anything_is_written() {
    let s = Scratch::new("m17", &["a", "b"]);
    let ceiling = soup_door::merge::CEILING_RECORDS as u32;
    let mut recs = vec![json!({"id": "R-1", "type": "region", "handle": "Everything", "boundary": "Inside: every idea this test writes. Outside: nothing at all."})];
    for i in 1..=ceiling {
        recs.push(json!({"id": format!("C-{i}"), "type": "idea", "state": "idea", "handle": format!("Idea {i}"), "edges": [{"kind": "in_region", "to": "R-1"}]}));
    }
    let o = s.import("a", &Value::Array(recs));
    assert_eq!(o.rc, 0, "{}{}", o.json()["records"].as_array().unwrap().iter().find(|r| r["disposition"] != "accepted").unwrap(), o.err);
    assert_eq!(s.import("b", &json!([{"id": "R-1", "type": "region", "handle": "Target", "boundary": "Inside: what the target holds. Outside: nothing at all."}])).rc, 0);
    let (a0, b0) = (files(&s.cauldron_dir("a")), files(&s.cauldron_dir("b")));
    let siblings = || -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(s.cauldron_dir("b").parent().unwrap()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    };
    let around = siblings();
    for extra in [&["--dry-run"][..], &[][..]] {
        let o = merge(&s, "a", "b", extra);
        assert_eq!(o.rc, 1, "{extra:?}: {}{}", o.out, o.err);
        let j = o.json();
        assert_eq!(j["refusal"], json!("too_large"), "{extra:?}: {j}");
        assert_eq!(j["detail"]["size"]["records"], json!(ceiling + 1), "{extra:?}: {j}");
        assert!(j["detail"]["size"]["estimate"]["seconds"].as_u64().unwrap() >= 10, "{extra:?}: {j}");
        assert!(o.err.contains("--allow-large") && o.err.contains("refuses every other writer") && o.err.contains("Nothing was read in full, held or written"), "{extra:?}: {}", o.err);
        assert!(files(&s.cauldron_dir("b")) == b0, "{extra:?}: the refusal changed the target");
        assert!(files(&s.cauldron_dir("a")) == a0, "{extra:?}: the refusal changed the source");
        assert_eq!(siblings(), around, "{extra:?}: a snapshot or anything else was made beside the target");
    }
    let o = merge(&s, "a", "b", &["--dry-run", "--allow-large"]);
    assert_eq!(o.rc, 0, "{}{}", &o.out[..o.out.len().min(2000)], o.err);
    assert!(o.err.contains("--allow-large") && o.err.contains("refuses every other writer meanwhile"), "{}", o.err);
    assert_eq!(o.json()["size"]["over_ceiling"], json!(true));
    assert!(files(&s.cauldron_dir("b")) == b0, "a dry run wrote to the target");
}

/// **How the ceiling's numbers are measured** (ignored by default; run by
/// hand): a source of `MERGE_SCALE_N` records (default 10,000) and
/// `MERGE_SCALE_R` records a ripple wrote (default 1,000), merged by the
/// `cauldron` at `MERGE_SCALE_BIN` (a release build; default this test's
/// own), timed and measured with `/usr/bin/time -l`, with
/// `--allow-large` above the ceiling. Prints one line.
#[test]
#[ignore]
fn m18_scale_measured_by_hand() {
    let n: u32 = std::env::var("MERGE_SCALE_N").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let r: usize = std::env::var("MERGE_SCALE_R").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000);
    let bin = std::env::var("MERGE_SCALE_BIN").unwrap_or_else(|_| CAULDRON.to_string());
    let s = Scratch::new("m18", &["a", "b"]);
    let regions = json!([
        {"id": "R-1", "type": "region", "handle": "Importing records",
         "boundary": "Inside: anything about importing records into the soup. Outside: R-2 for everything else."},
        {"id": "R-2", "type": "region", "handle": "Everything about edges",
         "boundary": "Inside: what edges carry and how they are kept. Outside: R-1 for importing records.", "edges": [{"kind": "narrows", "to": "R-1"}]},
    ]);
    let mut recs = regions.as_array().unwrap().clone();
    for i in 1..=n {
        let mut x = json!({"id": format!("C-{i}"), "type": "idea", "state": "idea", "handle": format!("Idea number {i} about records"),
                           "body": format!("see [[C-{}]] {}", i.saturating_sub(1).max(1), "x".repeat(180)), "edges": [{"kind": "in_region", "to": format!("R-{}", 1 + i % 2)}]});
        if i > 1 && i % 3 == 0 {
            x["edges"].as_array_mut().unwrap().push(json!({"kind": "ref", "to": format!("C-{}", i - 1)}));
        }
        recs.push(x);
    }
    let f = s.file("big.json", &Value::Array(recs).to_string());
    let o = s.run_with(&bin, &["soup", "import", f.to_str().unwrap(), "--env", "dev", "--cauldron", "a", "--remint"], &[], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(s.import("b", &regions).rc, 0);
    for i in 0..100 {
        s.dump("a", &format!("dump number {i} {}", "x".repeat(2000)));
    }
    let many: Vec<(String, String, String)> = (0..r).map(|i| ("idea".to_string(), format!("A ripple's idea {i}"), format!("body {i} {}", "y".repeat(180)))).collect();
    for (k, chunk) in many.chunks(2_000).enumerate() {
        let refs: Vec<(&str, &str, &str)> = chunk.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
        crate::ripples::ripple_write(&s.soup("a"), &format!("run-{k}"), "DM-1", "sonnet", &refs);
    }
    let t0 = Instant::now();
    let o = s.run_with("/usr/bin/time", &["-l", &bin, "merge", "a", "--into", "b", "--env", "dev", "--allow-large"], &[], &[]);
    let took = t0.elapsed();
    assert_eq!(o.rc, 0, "{}", &o.err[o.err.len().saturating_sub(2000)..]);
    let rss: u64 = o.err.lines().find(|l| l.contains("maximum resident set size")).and_then(|l| l.split_whitespace().next()).and_then(|x| x.parse().ok()).unwrap();
    let m = o.json();
    println!(
        "m18: {} records ({} a ripple's) and {} dumps merged in {:.1} s, {} MB resident at most; estimated {} s, {} MB",
        m["landed"]["records"], r, m["landed"]["dumps"], took.as_secs_f64(), rss >> 20, m["size"]["estimate"]["seconds"], m["size"]["estimate"]["memory_mb"]
    );
}

// ------------------------------------------------------------------ M13

/// A record of the main write that points at a record of the ripple part
/// (here a curator's task with a `ref` to a ripple's idea nobody decided):
/// the edge lands as the merge's third write, after both, and the idea is
/// unreviewed in the target.
#[test]
fn m13_an_edge_from_the_main_write_to_the_ripple_part_lands_after_both() {
    let f = fixture("m13", false);
    let s = &f.s;
    let o = s.import_with("a", &json!([{"id": "t", "type": "task", "state": "ready", "handle": "Follow up the ripple's first idea",
        "edges": [{"kind": "in_region", "to": "R-1"}, {"kind": "ref", "to": f.run_a[0], "note": "the idea this follows up"}]}]), &["--remint"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let task = o.json()["id_map"]["t"].as_str().unwrap().to_string();
    let cursor = s.cursor("b");
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let m = o.json();
    let idea = m["id_map"][&f.run_a[0]].as_str().unwrap().to_string();
    let t = s.get("b", m["id_map"][&task].as_str().unwrap());
    assert!(t["edges"].as_array().unwrap().iter().any(|e| e["kind"] == "ref" && e["to"] == json!(idea) && e["note"] == json!("the idea this follows up")), "{t}");
    assert_eq!(s.get("b", &idea)["unreviewed"], json!(true));
    let ch = s.run(&["soup", "changes", "--env", "dev", "--cauldron", "b", "--since", &cursor], &[]).json();
    let parts: Vec<Value> = ch["changes"].as_array().unwrap().iter().filter_map(|c| c.get("merge").map(|m| m["part"].clone())).collect();
    assert_eq!(parts, vec![json!("main"), json!("ripple"), json!("links")], "{ch}");
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(s.cauldron_dir("b").join("merges/MG-1/merge.json")).unwrap()).unwrap();
    assert_eq!(rec["soup_writes"].as_object().unwrap().len(), 3, "{rec}");
    assert_eq!(s.queue("b")["counts"]["unreviewed"], json!(4));
}

// ------------------------------------------------------------------ M6

/// What the target holds, as the reads answer it: its records (id, type,
/// handle) and its dumps (id, state).
#[cfg(feature = "fault-injection")]
fn holdings(s: &Scratch, c: &str) -> (Vec<(String, String, String)>, Vec<(String, String)>) {
    let mut n: Vec<(String, String, String)> = s
        .nodes(c)
        .iter()
        .map(|x| (x["id"].as_str().unwrap().to_string(), x["type"].as_str().unwrap().to_string(), x["handle"].as_str().unwrap().to_string()))
        .collect();
    n.sort();
    let mut d: Vec<(String, String)> = s.dumps(c).iter().map(|x| (x["id"].as_str().unwrap().to_string(), x["state"].as_str().unwrap_or("").to_string())).collect();
    d.sort();
    (n, d)
}

fn task(handle: &str) -> Value {
    json!([{"id": "t", "type": "task", "state": "ready", "handle": handle, "edges": [{"kind": "in_region", "to": "R-1"}]}])
}

/// The merge's id map, as saved.
#[cfg(feature = "fault-injection")]
fn saved_map(s: &Scratch, merge: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(s.cauldron_dir("b").join(format!("merges/{merge}/id-map.json"))).unwrap()).unwrap()
}

#[cfg(feature = "fault-injection")]
#[test]
fn m6_a_merge_cut_short_anywhere_is_unchanged_or_whole_after_recover() {
    // What an uninterrupted merge leaves.
    let r = fixture("m6-ref", false);
    let o = merge(&r.s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let whole = holdings(&r.s, "b");
    let whole_map = saved_map(&r.s, "MG-1");
    for step in soup_door::merge::fault::STEPS {
        let f = fixture(&format!("m6-{step}"), false);
        let s = &f.s;
        let before = holdings(s, "b");
        let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", step)]);
        assert_ne!(o.rc, 0, "{step}: the merge was not stopped: {}{}", o.out, o.err);
        assert!(o.err.contains(&format!("aborting after `{step}`")), "{step}: {}", o.err);
        if matches!(step, "snapshot" | "staged") {
            assert_eq!(holdings(s, "b"), before, "{step}: cut short before its journal, the target changed");
            let o = s.import_with("b", &task("x"), &["--remint"]);
            assert_eq!(o.rc, 0, "{step}: nothing is pending, so writes go on: {}", o.err);
            // What it staged from the source is swept by recover.
            let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]);
            assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
            if step == "staged" {
                assert_eq!(o.json()["merges_swept"], json!(["MG-1"]), "{step}: {}", o.out);
                assert!(!s.cauldron_dir("b").join("merges/MG-1").exists());
            }
            continue;
        }
        // Pending: every other writer of the target is refused, by name.
        let o = s.import_with("b", &task("x"), &["--remint"]);
        assert_ne!(o.rc, 0, "{step}");
        assert!(o.err.contains("merge MG-1 into cauldron `b` was cut short") && o.err.contains("cauldron soup recover"), "{step}: {}", o.err);
        // The refusal says what recover will do: abandon a merge cut before
        // it held the cauldron, finish one that held it.
        if step == "journal" {
            assert!(o.err.contains("recover abandons it") && !o.err.contains("lands in full"), "{step}: {}", o.err);
        } else {
            assert!(o.err.contains("it finishes the merge"), "{step}: {}", o.err);
        }
        let d = s.file("late.txt", "late");
        let o = s.run(&["soup", "dump", d.to_str().unwrap(), "--env", "dev", "--cauldron", "b"], &[]);
        assert_ne!(o.rc, 0, "{step}: a dump landed while the merge was pending");
        let o = merge(s, "a", "b", &[]);
        assert_ne!(o.rc, 0, "{step}: a second merge started over a pending one");
        let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]);
        assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
        let rec = o.json();
        if step == "journal" {
            // Cut short before it held the target: nothing of it landed, so
            // it is abandoned (the target unchanged), and runs again whole.
            assert_eq!(rec["merge"]["outcome"], json!("abandoned"), "{step}: {}", o.out);
            assert!(!rec["instruction"].as_str().unwrap().contains("FINISHED"), "{step}: the words say what happened: {}", o.out);
            assert_eq!(holdings(s, "b"), before, "{step}");
            let o = merge(s, "a", "b", &[]);
            assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
            assert_eq!(holdings(s, "b"), whole, "{step}");
            continue;
        }
        // Recover finishes it, whole.
        assert_eq!(rec["merge"]["outcome"], json!("finished"), "{step}: {}", o.out);
        assert_eq!(holdings(s, "b"), whole, "{step}: after recover the target is not what a whole merge leaves");
        let map = saved_map(s, "MG-1");
        assert_eq!((map["records"].clone(), map["dumps"].clone()), (whole_map["records"].clone(), whole_map["dumps"].clone()), "{step}");
        assert_eq!(s.queue("b")["counts"]["unreviewed"], json!(4), "{step}");
        // And afterwards: writes go on, and the merge is not run twice.
        let o = s.import_with("b", &task("x"), &["--remint"]);
        assert_eq!(o.rc, 0, "{step}: {}", o.err);
        let o = merge(s, "a", "b", &[]);
        assert!(o.out.contains("already_merged"), "{step}: {}", o.out);
    }
}

/// **An undo cut short at every step is finished, and says so meanwhile.**
/// `CAULDRON_UNDO_FAULT` aborts `cauldron merge --undo` after its marker,
/// its hold, its record of every file, each file it moves (`file:<n>`), and
/// the restore. Each time: `merge --status` says `undoing` (and how far),
/// `cauldron status` lists it with the command that finishes it, a 0.1.16
/// `cauldron` (with `CAULDRON_PREVIOUS_CLI`) is refused while the soup is
/// still held; then it is finished, in turn, by the same undo again, by
/// `soup recover`, and by the next writer (an import, which then lands),
/// and the target is byte for byte as it was before the merge (with the
/// import, for the last), `merge --status` says `undone`, and nothing is
/// listed as cut short.
#[cfg(feature = "fault-injection")]
#[test]
fn m15_an_undo_cut_short_at_any_step_is_finished_and_reported_meanwhile() {
    let old = std::env::var_os("CAULDRON_PREVIOUS_CLI");
    let mut steps: Vec<String> = ["marked", "held", "recorded"].iter().map(|x| x.to_string()).collect();
    steps.extend((1..=40).map(|n| format!("file:{n}")));
    steps.push("restored".into());
    let mut ran_past_the_last_file = false;
    for (i, step) in steps.iter().enumerate() {
        if step.starts_with("file:") && ran_past_the_last_file {
            continue;
        }
        let f = fixture(&format!("m15-{}", step.replace(':', "-")), false);
        let s = &f.s;
        let before = unlocked(&s.cauldron_dir("b"));
        let o = merge(s, "a", "b", &[]);
        assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
        let o = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", step)]);
        if o.rc == 0 && step.starts_with("file:") {
            // Fewer files to move than this step: the undo ran through.
            ran_past_the_last_file = true;
            assert_eq!(unlocked(&s.cauldron_dir("b")), before, "{step}");
            continue;
        }
        assert_ne!(o.rc, 0, "{step}: the undo was not stopped: {}{}", o.out, o.err);
        assert!(o.err.contains(&format!("aborting after `{step}`")), "{step}: {}", o.err);
        eprintln!("m15: undo cut after `{step}`, finished by {}", ["the undo again", "recover", "the next writer"][i % 3]);
        // Meanwhile: the truth, from every place that reports it.
        let st = s.run(&["merge", "--status", "MG-1", "--into", "b", "--env", "dev"], &[]);
        assert_eq!(st.rc, 0, "{step}: {}{}", st.out, st.err);
        let st = st.json();
        assert_eq!(st["state"], json!("undoing"), "{step}: {st}");
        let whole = s.run_with(CAULDRON, &["status", "--json"], &[], &[("CAULDRON_ENV", "dev")]);
        let whole: Value = serde_json::from_str(&whole.out).unwrap_or_else(|e| panic!("{step}: {e}: {}", whole.out));
        let cut = whole["merges_cut_short"].as_array().unwrap();
        assert!(cut.iter().any(|m| m["cauldron"] == "b" && m["state"] == "undoing" && m["merge"] == "MG-1"), "{step}: {whole}");
        let soup_held = st["undo"]["soup"].as_str().unwrap().starts_with("held");
        if let Some(old) = &old {
            if soup_held {
                let o = s.run_with(old.to_str().unwrap(), &["soup", "import", s.file("old.json", &task("old").to_string()).to_str().unwrap(), "--env", "dev", "--cauldron", "b", "--remint"], &[], &[]);
                assert_ne!(o.rc, 0, "{step}: the release before wrote into an undo in progress: {}{}", o.out, o.err);
            }
        }
        // Finished: by the undo again, by recover, or by the next writer.
        match i % 3 {
            0 => {
                let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
                assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
                let j = o.json();
                assert_eq!(j["outcome"], json!("undone"), "{step}: {j}");
                assert!(j["resumed"].as_str().unwrap().contains("the same undo"), "{step}: {j}");
                assert_eq!(unlocked(&s.cauldron_dir("b")), before, "{step}: not as before the merge");
            }
            1 => {
                let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]);
                assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
                let j = o.json();
                assert_eq!(j["merge_undo"]["outcome"], json!("finished"), "{step}: {j}");
                assert_eq!(j["outcome"], json!("undo-finished"), "{step}: {j}");
                assert_eq!(unlocked(&s.cauldron_dir("b")), before, "{step}: not as before the merge");
            }
            _ => {
                let o = s.import_with("b", &task("after"), &["--remint"]);
                assert_eq!(o.rc, 0, "{step}: the next writer finishes the undo and writes: {}{}", o.out, o.err);
                let handles: Vec<String> = s.nodes("b").iter().map(|n| n["handle"].as_str().unwrap_or("").to_string()).collect();
                assert!(handles.iter().any(|h| h == "after"), "{step}: {handles:?}");
                assert!(!handles.iter().any(|h| h.contains("A ripple's")), "{step}: the merge's records are gone: {handles:?}");
            }
        }
        let st = s.run(&["merge", "--status", "MG-1", "--into", "b", "--env", "dev"], &[]).json();
        assert_eq!(st["state"], json!("undone"), "{step}: {st}");
        let whole = s.run_with(CAULDRON, &["status", "--json"], &[], &[("CAULDRON_ENV", "dev")]);
        let whole: Value = serde_json::from_str(&whole.out).unwrap();
        assert_eq!(whole["merges_cut_short"], json!([]), "{step}: {whole}");
    }
    assert!(ran_past_the_last_file, "the steps reached past the last file the undo moves");
}

/// **After a crash, a write by the release before and a recover, an undo
/// removes nothing it cannot prove the merge created.** The merge is cut
/// after its last soup write (`ripple`) and after its record (`record`), so
/// the target is no longer held; the 0.1.16 `cauldron` (from
/// `CAULDRON_PREVIOUS_CLI`) imports a record (and, after `record`, dumps),
/// and the 0.1.16 `mcp` service (from `CAULDRON_PREVIOUS_MCP`, when set)
/// writes one through the door; this release's recover finishes the merge.
/// The undo is refused, naming those writes by count and id, and changes
/// nothing; the older writer's records are still there. Without a later
/// write, the same crash and recover leave a merge the undo puts back
/// exactly.
#[cfg(feature = "fault-injection")]
#[test]
fn m16_an_undo_never_removes_an_older_writers_records_after_a_crash_and_a_recover() {
    let Some(old) = std::env::var_os("CAULDRON_PREVIOUS_CLI") else {
        eprintln!("m16: skipped: set CAULDRON_PREVIOUS_CLI to a 0.1.16 `cauldron` to run it");
        return;
    };
    let old = old.to_str().unwrap().to_string();
    let old_mcp = std::env::var_os("CAULDRON_PREVIOUS_MCP").map(PathBuf::from);
    for step in ["ripple", "record"] {
        let f = fixture(&format!("m16-{step}"), false);
        let s = &f.s;
        let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", step)]);
        assert!(o.err.contains(&format!("aborting after `{step}`")), "{step}: {}{}", o.out, o.err);
        // The release before writes: the soup is no longer held.
        let o = s.run_with(&old, &["soup", "import", s.file("old.json", &task("Written by the release before").to_string()).to_str().unwrap(), "--env", "dev", "--cauldron", "b", "--remint"], &[], &[]);
        assert_eq!(o.rc, 0, "{step}: the 0.1.16 import: {}{}", o.out, o.err);
        let mut later_dumps = 0;
        if step == "record" {
            let d = s.file("old-dump.txt", "a note the release before kept");
            let o = s.run_with(&old, &["soup", "dump", d.to_str().unwrap(), "--env", "dev", "--cauldron", "b"], &[], &[]);
            assert_eq!(o.rc, 0, "{step}: the 0.1.16 dump: {}{}", o.out, o.err);
            later_dumps = 1;
        }
        let mut later_records = 1;
        if let Some(bin) = &old_mcp {
            s.grant("b");
            let svc = Service::start_bin(s, bin);
            let w = svc.call(s, "soup_put", json!({"cauldron": "b", "remint": true, "records": [{"id": "t", "type": "task", "state": "ready", "handle": "Written by the release before's service", "edges": [{"kind": "in_region", "to": "R-1"}]}]}));
            assert_eq!(w["isError"], json!(false), "{step}: the 0.1.16 service's soup_put: {w}");
            svc.stop();
            s.revoke("b");
            later_records += 1;
        }
        let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]);
        assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
        assert_eq!(o.json()["merge"]["outcome"], json!("finished"), "{step}: {}", o.out);
        let written = unlocked(&s.cauldron_dir("b"));
        let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
        assert_eq!(o.rc, 1, "{step}: the undo must refuse: {}{}", o.out, o.err);
        let j = o.json();
        assert_eq!(j["refusal"], json!("changed_since"), "{step}: {j}");
        let ch = &j["changed_outside_the_merge"];
        assert_eq!(ch["records"]["added"], json!(later_records), "{step}: {j}");
        assert_eq!(ch["soup_txs"]["added"], json!(later_records), "{step}: {j}");
        if later_dumps > 0 {
            assert_eq!(ch["dumps"]["added"], json!(later_dumps), "{step}: {j}");
        }
        assert!(o.err.contains("never removes anything it cannot prove the merge created") && o.err.contains("is not offered"), "{step}: {}", o.err);
        assert!(!o.err.contains("a note the release before kept") && !o.err.contains("Written by the release before"), "{step}: ids, never text: {}", o.err);
        assert_eq!(unlocked(&s.cauldron_dir("b")), written, "{step}: a refused undo changed the target");
        let handles: Vec<String> = s.nodes("b").iter().map(|n| n["handle"].as_str().unwrap_or("").to_string()).collect();
        assert!(handles.iter().any(|h| h == "Written by the release before"), "{step}: {handles:?}");
        assert_eq!(s.dumps("b").len(), 2 + 3 + later_dumps, "{step}");
    }
    // The same crash and recover with no later write: undone exactly.
    let f = fixture("m16-clean", false);
    let s = &f.s;
    let before = unlocked(&s.cauldron_dir("b"));
    let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", "record")]);
    assert!(o.err.contains("aborting after `record`"), "{}", o.err);
    assert_eq!(s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]).rc, 0);
    let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(unlocked(&s.cauldron_dir("b")), before);
}

/// **A merge recover can neither finish nor put back is not a dead end**,
/// and reads say a merge is pending. Cut after `held`: `soup nodes` says
/// `merge_pending`; the staged blobs and the snapshot are then removed, so
/// recover refuses, saying the snapshot is gone and naming
/// `--abandon-merge`; `soup recover --abandon-merge` lifts the hold, lists
/// what landed, and writes go on. A snapshot cut short while it was being
/// taken (`<cauldron>.taking-<pid>`) is swept by recover, and a complete one
/// no merge used is listed, not removed.
#[cfg(feature = "fault-injection")]
#[test]
fn m20_a_merge_recover_cannot_settle_is_abandoned_on_request_and_reads_say_it_is_pending() {
    let f = fixture("m20", false);
    let s = &f.s;
    let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", "held")]);
    assert!(o.err.contains("aborting after `held`"), "{}", o.err);
    let o = s.run(&["soup", "nodes", "--env", "dev", "--cauldron", "b"], &[]);
    assert_eq!(o.rc, 0, "{}", o.err);
    assert_eq!(o.json()["merge_pending"]["merge"], json!("MG-1"), "{}", o.out);
    let j: Value = serde_json::from_str(&std::fs::read_to_string(s.cauldron_dir("b").join("merges/MG-1/journal.json")).unwrap()).unwrap();
    let snap = PathBuf::from(j["snapshot"]["dir"].as_str().unwrap());
    std::fs::remove_dir_all(s.cauldron_dir("b").join("merges/MG-1/staged")).unwrap();
    std::fs::remove_dir_all(&snap).unwrap();
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]);
    assert_ne!(o.rc, 0, "{}{}", o.out, o.err);
    let said = format!("{}{}", o.out, o.err);
    assert!(said.contains("snapshot") && said.contains("is not there any more") && said.contains("--abandon-merge"), "{said}");
    assert_ne!(s.import_with("b", &task("refused"), &["--remint"]).rc, 0, "still held");
    // Leftovers of a snapshot cut short, and a complete one nobody used.
    let beside = s.cauldron_dir("b").parent().unwrap().to_path_buf();
    std::fs::create_dir_all(beside.join("b.taking-424242/files/soup")).unwrap();
    std::fs::write(beside.join("b.taking-424242/files/soup/ws.db"), b"partial").unwrap();
    let unused = beside.join("b.MG-7-before-20260101T000000000Z");
    std::fs::create_dir_all(&unused).unwrap();
    std::fs::write(unused.join("manifest.json"), b"{}").unwrap();
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-merge"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let r = o.json();
    assert_eq!(r["merge"]["outcome"], json!("abandoned-on-request"), "{r}");
    assert!(r["merge"]["landed"]["soup_writes"].as_object().unwrap().is_empty(), "nothing of it had landed at `held`: {r}");
    assert!(r["merges_swept"].as_array().unwrap().iter().any(|x| x == "b.taking-424242"), "{r}");
    assert!(!beside.join("b.taking-424242").exists());
    assert!(r["snapshots_unused"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().ends_with("b.MG-7-before-20260101T000000000Z")), "{r}");
    assert!(unused.is_dir(), "a complete snapshot is listed, never removed");
    assert_eq!(s.import_with("b", &task("after"), &["--remint"]).rc, 0, "writes go on");
    let o = s.run(&["soup", "nodes", "--env", "dev", "--cauldron", "b"], &[]);
    assert!(o.json().get("merge_pending").is_none(), "{}", o.out);
    let st = s.run(&["merge", "--status", "MG-1", "--into", "b", "--env", "dev"], &[]).json();
    assert_eq!(st["state"], json!("abandoned"), "{st}");
}

/// **A live region lock in the target is seen by the plan.** `a` merged
/// into `b`, then `a` gains a record in a region `b` already holds (as
/// `R-3`), and a live ripple run holds `R-3` in `b`: the dry run is blocked
/// naming the region and the wait, before anything is held; once the run
/// lets go, it is not.
#[test]
fn m19_a_target_region_a_live_run_holds_blocks_the_plan() {
    let f = fixture("m19", false);
    let s = &f.s;
    assert_eq!(merge(s, "a", "b", &[]).rc, 0);
    assert_eq!(s.import_with("a", &task("A new task in the first region"), &["--remint"]).rc, 0);
    let c = s.env().find_cauldron("b").unwrap().unwrap();
    let mut t = env::region_locks::load(&c).unwrap();
    let holder = env::region_locks::Holder { kind: "ripple".into(), run: "run-live".into(), dump: None };
    t.acquire(&holder, "a test", "ws", &["R-3".to_string()], &[], env::region_locks::now_ms(), 600_000).unwrap();
    env::region_locks::save(&c, &t).unwrap();
    let o = merge(s, "a", "b", &["--dry-run"]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    let p = o.json();
    assert_eq!(p["outcome"], json!("blocked"), "{p}");
    assert!(p["blocking"].to_string().contains("R-3") && p["blocking"].to_string().contains("ripples status"), "{p}");
    t.release("run-live");
    env::region_locks::save(&c, &t).unwrap();
    let o = merge(s, "a", "b", &["--dry-run"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
}

/// A merge that cannot be finished (here: the blobs it staged are gone)
/// is not left pending forever: recover puts the target back exactly as the
/// snapshot holds it, says so and why, and writes go on.
#[cfg(feature = "fault-injection")]
#[test]
fn m6b_a_merge_recover_cannot_finish_is_put_back_from_its_snapshot() {
    let f = fixture("m6b", false);
    let s = &f.s;
    let before = unlocked(&s.cauldron_dir("b"));
    let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", "held")]);
    assert!(o.err.contains("aborting after `held`"), "{}", o.err);
    std::fs::remove_dir_all(s.cauldron_dir("b").join("merges/MG-1/staged")).unwrap();
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let m = &o.json()["merge"];
    assert_eq!(m["outcome"], json!("restored"), "{m}");
    assert!(m["note"].as_str().unwrap().contains("put cauldron `b` back exactly as it was before the merge"), "{m}");
    assert!(o.json()["instruction"].as_str().unwrap().contains("put cauldron `b` back"), "{}", o.out);
    let after = unlocked(&s.cauldron_dir("b"));
    let differ: Vec<&String> = before.keys().chain(after.keys()).filter(|k| before.get(*k) != after.get(*k)).collect();
    assert!(differ.is_empty(), "these differ from before the merge: {differ:?}");
    let o = s.import_with("b", &task("after"), &["--remint"]);
    assert_eq!(o.rc, 0, "{}", o.err);
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "the merge runs again: {}{}", o.out, o.err);
}

/// **The release before, writing during every pending state** (with
/// `CAULDRON_PREVIOUS_CLI` naming a 0.1.16 `cauldron`): while the merge holds
/// the target, its import and its dump are refused by the spine's halted
/// state, which it honours; where the merge does not hold a store (before
/// the hold, the soup after the records' commit) its write lands, and
/// `recover` then abandons or finishes the merge with an id map read back
/// from what landed, never a wrong one, and is never stuck.
#[cfg(feature = "fault-injection")]
#[test]
fn m9_the_release_before_writing_while_a_merge_is_pending() {
    let Some(old) = std::env::var_os("CAULDRON_PREVIOUS_CLI") else {
        eprintln!("m9: skipped: set CAULDRON_PREVIOUS_CLI to a 0.1.16 `cauldron` to run it");
        return;
    };
    let old = old.to_str().unwrap().to_string();
    let r = fixture("m9-ref", false);
    assert_eq!(merge(&r.s, "a", "b", &[]).rc, 0);
    let whole_map = saved_map(&r.s, "MG-1");
    for step in ["journal", "held", "dump", "dumps", "soup", "ripple", "record"] {
        let f = fixture(&format!("m9-{step}"), false);
        let s = &f.s;
        let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", step)]);
        assert!(o.err.contains(&format!("aborting after `{step}`")), "{step}: {}", o.err);
        let file = s.file(&format!("old-{step}.json"), &task(&format!("Written by 0.1.16 after `{step}`")).to_string());
        let imp = s.run_with(&old, &["soup", "import", file.to_str().unwrap(), "--env", "dev", "--cauldron", "b", "--remint"], &[], &[]);
        let d = s.file(&format!("old-{step}.txt"), "a dump 0.1.16 wrote while the merge was pending");
        let dmp = s.run_with(&old, &["soup", "dump", d.to_str().unwrap(), "--env", "dev", "--cauldron", "b"], &[], &[]);
        println!("m9 {step}: 0.1.16 import rc {} ({}), dump rc {} ({})", imp.rc, imp.err.trim(), dmp.rc, dmp.err.trim());
        let soup_held = matches!(step, "held" | "dump" | "dumps" | "soup");
        let dumps_held = matches!(step, "held" | "dump" | "dumps" | "soup" | "ripple");
        assert_eq!(imp.rc != 0, soup_held, "{step}: 0.1.16 import: {}{}", imp.out, imp.err);
        assert_eq!(dmp.rc != 0, dumps_held, "{step}: 0.1.16 dump: {}{}", dmp.out, dmp.err);
        if soup_held {
            assert!(imp.err.contains("HALTED") || imp.err.contains("halted"), "{step}: {}", imp.err);
            // Its own `soup recover`, which its refusal names, cannot lift the
            // hold or finish the merge; it changes nothing that matters.
            let rec = s.run_with(&old, &["soup", "recover", "--env", "dev", "--cauldron", "b"], &[], &[]);
            println!("m9 {step}: 0.1.16 recover rc {}: {}", rec.rc, rec.out.lines().filter(|l| l.contains("instruction") || l.contains("outcome")).collect::<Vec<_>>().join(" "));
            let again = s.run_with(&old, &["soup", "import", file.to_str().unwrap(), "--env", "dev", "--cauldron", "b", "--remint"], &[], &[]);
            assert_ne!(again.rc, 0, "{step}: 0.1.16 wrote after its own recover: {}", again.out);
        }
        if dumps_held {
            assert!(dmp.err.contains("merge MG-1 into cauldron `b`"), "{step}: the refusal names the merge: {}", dmp.err);
        }
        let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]);
        assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
        let outcome = o.json()["merge"]["outcome"].clone();
        let mut id = "MG-1".to_string();
        if step == "journal" {
            assert_eq!(outcome, json!("abandoned"), "{step}: {}", o.out);
            let o = merge(s, "a", "b", &[]);
            assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
            id = o.json()["merge"].as_str().unwrap().to_string();
        } else {
            assert_eq!(outcome, json!("finished"), "{step}: {}", o.out);
        }
        // The id map names exactly the records the merge carried.
        let map = saved_map(s, &id);
        for (src, new) in map["records"].as_object().unwrap() {
            let rec = s.get("b", new.as_str().unwrap());
            if let Some(from) = rec["meta"].get("merged_from") {
                assert_eq!(from["id"], json!(src), "{step}: the id map names {new} for {src}: {rec}");
            }
        }
        if step != "journal" {
            assert_eq!(map["records"], whole_map["records"], "{step}");
        }
        let dumps = s.dumps("b");
        for new in map["dumps"].as_object().unwrap().values() {
            assert!(dumps.iter().any(|d| d["id"] == *new), "{step}: {new} is not a dump of b");
        }
        assert_eq!(s.queue("b")["counts"]["unreviewed"], json!(4), "{step}");
        // Never stuck: this release writes on.
        let o = s.import_with("b", &task("after recover"), &["--remint"]);
        assert_eq!(o.rc, 0, "{step}: {}", o.err);
        let o = s.run_with(&old, &["soup", "import", file.to_str().unwrap(), "--env", "dev", "--cauldron", "b", "--remint", "--allow-duplicate"], &[], &[]);
        assert_eq!(o.rc, 0, "{step}: 0.1.16 writes on too: {}", o.err);
    }
}

// ------------------------------------------------------------------ M7

/// The release before (0.1.16) reads the merged target: every read verb
/// answers, and a merged record reads as an ordinary one.
#[test]
fn m7_the_release_before_reads_the_merged_target() {
    let Some(old) = std::env::var_os("CAULDRON_PREVIOUS_CLI") else {
        eprintln!("m7: skipped: set CAULDRON_PREVIOUS_CLI to a 0.1.16 `cauldron` to run it");
        return;
    };
    let old = old.to_str().unwrap().to_string();
    let f = fixture("m7", true);
    let s = &f.s;
    let o = s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let m = printed_json(&o.out);
    let b_after = files(&s.cauldron_dir("b"));
    let new_is = m["id_map"]["IS-1"].as_str().unwrap().to_string();
    let new_cl = m["id_map"]["CL-1"].as_str().unwrap().to_string();
    let new_dm = m["dump_map"]["DM-2"].as_str().unwrap().to_string();
    for args in [
        vec!["soup", "regions", "--env", "dev", "--cauldron", "b"],
        vec!["soup", "nodes", "--env", "dev", "--cauldron", "b", "--limit", "100"],
        vec!["soup", "get", new_is.as_str(), new_cl.as_str(), "--env", "dev", "--cauldron", "b"],
        vec!["soup", "search", "ripple", "--env", "dev", "--cauldron", "b"],
        vec!["soup", "region", "R-3", "--env", "dev", "--cauldron", "b"],
        vec!["soup", "dumps", "--env", "dev", "--cauldron", "b"],
        vec!["soup", "dump-get", new_dm.as_str(), "--env", "dev", "--cauldron", "b"],
        vec!["soup", "changes", "--env", "dev", "--cauldron", "b"],
        vec!["ripples", "review", "--env", "dev", "--cauldron", "b"],
    ] {
        let o = s.run_with(&old, &args, &[], &[]);
        println!("m7 {}: rc {}", args.join(" "), o.rc);
        assert_eq!(o.rc, 0, "{args:?}: {}{}", o.out, o.err);
        let _: Value = serde_json::from_str(&o.out).unwrap_or_else(|e| panic!("{args:?}: not JSON ({e}): {}", o.out));
    }
    let o = s.run_with(&old, &["soup", "get", &new_is, "--env", "dev", "--cauldron", "b"], &[], &[]);
    assert_eq!(o.json()["records"][0]["meta"]["merged_from"]["id"], json!("IS-1"), "{}", o.out);
    // The records a ripple wrote that nobody decided read as unreviewed to
    // the release before too (they land in a write of the ripple class, which
    // is what it keys on): its queue counts what this release's counts, and
    // its `get` marks them.
    let new_q = s.queue("b");
    let o = s.run_with(&old, &["ripples", "review", "--env", "dev", "--cauldron", "b"], &[], &[]);
    let old_q = o.json();
    println!("m7: 0.1.16 queue {} / this release {}", old_q["counts"], new_q["counts"]);
    assert_eq!(old_q["counts"]["unreviewed"], new_q["counts"]["unreviewed"], "{old_q}");
    assert_eq!(old_q["counts"]["unreviewed"], json!(2), "{old_q}");
    let un = m["id_map"][&f.run_a[0]].as_str().unwrap().to_string();
    let o = s.run_with(&old, &["soup", "get", &un, "--env", "dev", "--cauldron", "b"], &[], &[]);
    assert_eq!(o.json()["records"][0]["unreviewed"], json!(true), "{}", o.out);
    let o = s.run_with(&old, &["soup", "nodes", "--review", "pending", "--env", "dev", "--cauldron", "b"], &[], &[]);
    assert_eq!(o.json()["nodes"].as_array().map(Vec::len), Some(2), "{}", o.out);
    let ap = m["id_map"][&f.run_b[0]].as_str().unwrap().to_string();
    let o = s.run_with(&old, &["soup", "get", &ap, "--env", "dev", "--cauldron", "b"], &[], &[]);
    assert!(o.json()["records"][0].get("unreviewed").is_none(), "an approved record is not unreviewed to it either: {}", o.out);
    let o = s.run_with(&old, &["soup", "get", &new_cl, "--env", "dev", "--cauldron", "b"], &[], &[]);
    assert_eq!(o.json()["records"][0]["claims"].as_array().map(Vec::len), Some(2), "{}", o.out);
    // Its reads wrote nothing.
    assert!(files(&s.cauldron_dir("b")) == b_after, "the older release's reads wrote to the merged target");
}

// ------------------------------------------------------------------ M21

/// Every way out a refusal can name, as a command.
fn way_out(text: &str) -> Option<&'static str> {
    // An undo that can still be finished: recover finishes it.
    if text.contains("can still be finished") || text.contains("can still be settled") {
        return Some("");
    }
    for flag in ["--abandon-undo", "--abandon-merge"] {
        if text.contains(&format!("cauldron soup recover --env dev --cauldron b {flag}")) {
            return Some(flag);
        }
    }
    text.contains("cauldron soup recover --env dev --cauldron b").then_some("")
}

/// One probe's outcome, short, and whether it is acceptable: it worked, it
/// was refused for a reason of its own (already merged, already undone,
/// written since), or it named a way out. A bare I/O error, or a refusal
/// naming no command, is not.
fn probe(o: &Out) -> (String, bool) {
    let text = format!("{}\n{}", o.out, o.err);
    if o.rc == 0 {
        return ("ok".into(), true);
    }
    let code = serde_json::from_str::<Value>(&o.out).ok().and_then(|v| v["refusal"].as_str().map(str::to_string)).unwrap_or_else(|| "err".into());
    // Refused for a reason of its own, saying the cauldron stays writable
    // (an undo dropped or impossible: "writes go on").
    let own = (["already_merged", "changed_since", "not_found"].contains(&code.as_str()) || text.contains("writes go on")) && !text.contains("(os error");
    match way_out(&text) {
        Some(f) => (format!("refused, names recover{}", if f.is_empty() { String::new() } else { format!(" {f}") }), true),
        None if own => (format!("refused ({code})"), true),
        None => (format!("STUCK ({code}): {}", text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(300).collect::<String>()), false),
    }
}

fn beside(s: &Scratch, c: &str, infix: &str) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(s.data().join("cauldrons"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!("{c}.MG-1{infix}")))
        .collect();
    v.sort();
    v.pop()
}

/// **No phase of a merge or an undo, with any file it depends on missing,
/// truncated or changed, leaves the target unwritable without hand-editing
/// files** (B1 and its siblings). For each cell: the merge (or the undo) is
/// cut short at the phase, the file is taken away or damaged; then an
/// import, the undo again, a new merge, `soup recover` and `soup recover
/// --abandon-merge` must each work or refuse naming a way out (never a bare
/// I/O error); and following the named way out, by commands only, must get
/// an import to land. Where the undo's marker survives, `--abandon-undo`
/// returns the target to exactly what the merge left (same records, same
/// dumps) unless it says what it could not return. The table is printed.
#[cfg(feature = "fault-injection")]
#[test]
fn m21_no_phase_with_any_file_missing_leaves_the_target_unwritable() {
    let undo_phases = ["marked", "held", "recorded", "file:2", "file:4", "file:7", "restored"];
    let undo_cases = [
        "snapshot moved",
        "snapshot manifest gone",
        "snapshot file changed",
        "marker gone",
        "marker truncated",
        "record gone",
        "aside gone",
        "snapshot and aside gone",
        "marker and snapshot gone",
    ];
    let merge_phases = ["journal", "held", "dumps", "soup", "ripple"];
    let merge_cases = ["snapshot moved", "journal gone", "journal truncated", "staged gone"];
    let mut cells: Vec<(bool, &str, &str)> = Vec::new();
    for p in undo_phases {
        for c in undo_cases {
            cells.push((true, p, c));
        }
    }
    for p in merge_phases {
        for c in merge_cases {
            cells.push((false, p, c));
        }
    }
    let cell = |(undo, phase, case): (bool, &str, &str)| -> (String, bool) {
        let tag = format!("m21-{}-{}-{}", if undo { "u" } else { "m" }, phase.replace(':', ""), case.replace(' ', ""));
        let f = fixture(&tag, false);
        let s = &f.s;
        let b = s.cauldron_dir("b");
        let away = s.0.join("away");
        std::fs::create_dir_all(&away).unwrap();
        let mv = |p: &Path, name: &str| -> bool { p.exists() && std::fs::rename(p, away.join(name)).is_ok() };
        let truncate = |p: &Path| -> bool {
            let Ok(t) = std::fs::read(p) else { return false };
            std::fs::write(p, &t[..t.len().min(17)]).is_ok()
        };
        let (merged_nodes, merged_dumps);
        let (pre_nodes, pre_dumps) = (handles_of(s), s.dumps("b").len());
        if undo {
            assert_eq!(merge(s, "a", "b", &[]).rc, 0);
            merged_nodes = handles_of(s);
            merged_dumps = s.dumps("b").len();
            let o = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", phase)]);
            if o.rc == 0 {
                return (format!("| undo {phase} | {case} | ran through (no such step) |"), true);
            }
        } else {
            merged_nodes = Vec::new();
            merged_dumps = 0;
            let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", phase)]);
            assert_ne!(o.rc, 0, "{tag}: {}", o.err);
        }
        let snap = beside(s, "b", "-before-");
        let aside = beside(s, "b", "-undone-");
        let applied = match case {
            "snapshot moved" => snap.as_deref().is_some_and(|p| mv(p, "snapshot")),
            "snapshot manifest gone" => snap.as_deref().is_some_and(|p| mv(&p.join("manifest.json"), "manifest.json")),
            "snapshot file changed" => snap.as_deref().is_some_and(|p| {
                let f = p.join("files/soup/spine.db");
                let mut t = std::fs::read(&f).unwrap();
                t.extend_from_slice(b"bit rot");
                std::fs::write(&f, t).is_ok()
            }),
            "marker gone" => mv(&b.join("merge-undoing.json"), "marker"),
            "marker truncated" => truncate(&b.join("merge-undoing.json")),
            "record gone" => mv(&b.join("merges/MG-1/merge.json"), "merge.json"),
            "aside gone" => aside.as_deref().is_some_and(|p| mv(p, "aside")),
            "snapshot and aside gone" => {
                let a = aside.as_deref().is_some_and(|p| mv(p, "aside"));
                snap.as_deref().is_some_and(|p| mv(p, "snapshot")) || a
            }
            "marker and snapshot gone" => {
                let m = mv(&b.join("merge-undoing.json"), "marker");
                snap.as_deref().is_some_and(|p| mv(p, "snapshot")) && m
            }
            "journal gone" => mv(&b.join("merges/MG-1/journal.json"), "journal.json"),
            "journal truncated" => truncate(&b.join("merges/MG-1/journal.json")),
            "staged gone" => mv(&b.join("merges/MG-1/staged"), "staged"),
            _ => unreachable!(),
        };
        let what = if undo { format!("undo {phase}") } else { format!("merge {phase}") };
        if !applied {
            // Nothing of the kind at that phase: the cell is the phase alone.
        }
        let mut row = vec![what.clone(), format!("{case}{}", if applied { "" } else { " (n/a: not there at this phase)" })];
        let mut ok = true;
        let probes: [(&str, Vec<&str>); 5] = [
            ("import", vec![]),
            ("undo again", vec!["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"]),
            ("new merge", vec!["merge", "a", "--into", "b", "--env", "dev"]),
            ("recover", vec!["soup", "recover", "--env", "dev", "--cauldron", "b"]),
            ("recover --abandon-merge", vec!["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-merge"]),
        ];
        let mut said = Vec::new();
        let mut remerged: Option<String> = None;
        for (name, args) in &probes {
            let o = if *name == "import" { s.import_with("b", &task(&format!("probe {tag}")), &["--remint"]) } else { s.run(args, &[]) };
            if *name == "new merge" && o.rc == 0 {
                // The merge that is there now is this one (another id).
                remerged = serde_json::from_str::<Value>(&o.out).ok().and_then(|v| v["merge"].as_str().map(str::to_string));
            }
            let (short, good) = probe(&o);
            ok &= good;
            said.push(format!("{name}: {short}"));
        }
        row.push(said.join("; "));
        // The way out, by commands only.
        let mut steps = Vec::new();
        let mut writable = false;
        let mut abandoned: Option<Value> = None;
        let mut last_flag: &str = "";
        for _ in 0..4 {
            let o = s.import_with("b", &task(&format!("after {tag}")), &["--remint"]);
            if o.rc == 0 {
                writable = true;
                break;
            }
            // The flag the import names, else the one the last recover named.
            let flag = match way_out(&format!("{}\n{}", o.out, o.err)) {
                Some(f) if !f.is_empty() => f,
                _ => last_flag,
            };
            let mut args = vec!["soup", "recover", "--env", "dev", "--cauldron", "b"];
            if !flag.is_empty() {
                args.push(flag);
            }
            // The owner's word for the abandons (they are the owner act `merge`).
            let r = s.run_with(CAULDRON, &args, &[], &[("CAULDRON_OWNER_ACTS", "merge")]);
            if flag == "--abandon-undo" && r.rc == 0 {
                abandoned = Some(printed_json(&r.out)["merge_undo"].clone());
            }
            if r.rc != 0 {
                last_flag = way_out(&format!("{}\n{}", r.out, r.err)).unwrap_or("");
            }
            let (short, _) = probe(&r);
            steps.push(format!("recover{} -> {}", if flag.is_empty() { String::new() } else { format!(" {flag}") }, short));
            if r.rc != 0 {
                // Its refusal names the next step; the next import says it too.
                let (_, good) = probe(&r);
                ok &= good;
            }
        }
        ok &= writable;
        let mut exact = String::new();
        if let Some(u) = &abandoned {
            exact = format!(" (abandon-undo: {}, returned {})", u["outcome"].as_str().unwrap_or("?"), u["returned"]);
        }
        // The END STATE: the records, the dumps and `--status` on one side,
        // as merged or as before the merge, never one store from each; and a
        // new dump never takes an id a record cites. Only where two things
        // were taken away by hand (the snapshot and the copies the undo moved
        // aside, or the marker and the snapshot) can neither side be had: then
        // the abandon must say TORN, and the id rule still holds.
        if undo {
            // A cauldron `--status` calls torn is settled by the owner first.
            let st0 = s.run(&["merge", "--status", "MG-1", "--into", "b", "--env", "dev"], &[]);
            if serde_json::from_str::<Value>(&st0.out).is_ok_and(|v| v["state"] == "torn") {
                let r = s.run_with(CAULDRON, &["soup", "recover", "--env", "dev", "--cauldron", "b", "--settle-merge", "MG-1"], &[], &[("CAULDRON_OWNER_ACTS", "merge")]);
                exact.push_str(&format!("; settle: rc {}", r.rc));
            }
            let mut now = handles_of(s);
            now.retain(|h| !h.starts_with("after ") && !h.starts_with("probe "));
            let nd = s.dumps("b").len();
            let id = remerged.clone().unwrap_or_else(|| "MG-1".into());
            let st = s.run(&["merge", "--status", &id, "--into", "b", "--env", "dev"], &[]);
            let state = serde_json::from_str::<Value>(&st.out).ok().and_then(|v| v["state"].as_str().map(str::to_string)).unwrap_or_else(|| "none".into());
            let side = if now == merged_nodes && nd == merged_dumps {
                "merged"
            } else if now == pre_nodes && nd == pre_dumps {
                "before the merge"
            } else {
                "MIXED"
            };
            let status_agrees = match side {
                // A record taken away by hand: the merge is whole and
                // `--status` says it can no longer be undone.
                "merged" => state == "merged" || (case == "record gone" && state == "partial"),
                "before the merge" => state != "merged" && state != "torn",
                _ => state == "torn" || state == "undoing",
            };
            let cited: Vec<String> = s.nodes("b").iter().flat_map(|n| s.get("b", n["id"].as_str().unwrap_or(""))["records"][0]["meta"]["cites"].as_array().cloned().unwrap_or_default()).filter_map(|c| c["id"].as_str().map(str::to_string)).collect();
            let fresh = s.dump("b", &format!("a note after the cell {tag}"));
            let reused = cited.contains(&fresh);
            let double = case == "snapshot and aside gone" || case == "marker and snapshot gone";
            let said_torn = abandoned.as_ref().is_some_and(|u| u["outcome"] == "undo-abandoned-torn") || state == "torn";
            let consistent = side != "MIXED" && status_agrees;
            ok &= !reused && (consistent || (double && said_torn));
            exact.push_str(&format!(
                "; end: {side}, status {state}, new dump {fresh}{}{}",
                if reused { " (REUSED A CITED ID)" } else { "" },
                if consistent { "" } else if double && said_torn { " (torn, said)" } else { " (INCONSISTENT)" }
            ));
        }
        let reads = s.run(&["soup", "nodes", "--env", "dev", "--cauldron", "b"], &[]).rc == 0;
        ok &= reads;
        row.push(format!("{}{}", if steps.is_empty() { "nothing needed".to_string() } else { steps.join(", ") }, exact));
        row.push(if writable { "writable".into() } else { "NOT WRITABLE".into() });
        (format!("| {} |", row.join(" | ")), ok)
    };
    let rows: Vec<(String, bool)> = std::thread::scope(|sc| {
        let cell = &cell;
        cells
            .chunks(cells.len().div_ceil(6))
            .map(|chunk| sc.spawn(move || chunk.iter().map(|c| cell(*c)).collect::<Vec<_>>()))
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    eprintln!("| phase | missing | probes | way out | end |\n|---|---|---|---|---|");
    for (r, _) in &rows {
        eprintln!("{r}");
    }
    let bad: Vec<&String> = rows.iter().filter(|r| !r.1).map(|r| &r.0).collect();
    assert!(bad.is_empty(), "{} cell(s) failed:\n{}", bad.len(), bad.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
}

fn handles_of(s: &Scratch) -> Vec<String> {
    let mut v: Vec<String> = s.nodes("b").iter().map(|n| n["handle"].as_str().unwrap_or("").to_string()).collect();
    v.sort();
    v
}

// ------------------------------------------------------------------ M22

/// **B1 and M1 as the inspection ran them.** An undo cut short after it
/// began putting files back, its snapshot then moved away: every writer,
/// recover, `--abandon-merge`, the undo again and a new merge are refused
/// naming `soup recover --abandon-undo`; that returns what the undo had put
/// back (the records are as the merge left them), lifts its hold, and says
/// what it kept; the next import lands; with the snapshot back, the merge
/// can be undone after all. Cut one step earlier (held, nothing moved), the
/// next writer says the undo was DROPPED and why, never that the target's
/// own records are someone else's, and names no path that is not there.
#[cfg(feature = "fault-injection")]
#[test]
fn m22_an_undo_whose_snapshot_went_is_abandoned_on_request_and_never_dropped_silently() {
    // B1, cut after the second file.
    let f = fixture("m22b", false);
    let s = &f.s;
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0);
    let snap = PathBuf::from(o.json()["snapshot"]["dir"].as_str().unwrap());
    let merged = handles_of(s);
    let merged_dumps = s.dumps("b").len();
    let o = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", "file:2")]);
    assert_ne!(o.rc, 0);
    let away = s.0.join("snapshot-moved-away");
    std::fs::rename(&snap, &away).unwrap();
    let way = "cauldron soup recover --env dev --cauldron b --abandon-undo";
    for (what, o) in [
        ("import", s.import_with("b", &task("x"), &["--remint"])),
        ("recover", s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[])),
        ("abandon-merge", s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-merge"], &[])),
        ("undo again", s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[])),
        ("new merge", merge(s, "a", "b", &[])),
    ] {
        let text = format!("{}{}", o.out, o.err);
        assert_ne!(o.rc, 0, "{what}: {text}");
        assert!(text.contains(way) && text.contains("is not there any more") && !text.contains("(os error"), "{what}: {text}");
    }
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-undo"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let r = printed_json(&o.out);
    assert_eq!(r["outcome"], "undo-abandoned", "{r}");
    let u = &r["merge_undo"];
    assert_eq!(u["not_restored"], json!([]), "{u}");
    assert!(u["returned"].as_u64().unwrap() >= 2, "{u}");
    let note = u["note"].as_str().unwrap();
    assert!(note.contains("exactly as merge MG-1 left it") && note.contains("once its snapshot is back exactly as it was"), "{note}");
    assert!(Path::new(u["set_aside_in"].as_str().unwrap()).join("merge-undoing.json").is_file(), "{u}");
    assert_eq!(handles_of(s), merged);
    assert_eq!(s.dumps("b").len(), merged_dumps);
    let st = s.run(&["merge", "--status", "MG-1", "--into", "b", "--env", "dev"], &[]).json();
    assert_eq!(st["state"], "merged", "{st}");
    assert_eq!(s.import_with("b", &task("after the abandon"), &["--remint"]).rc, 0);
    // The snapshot back: the merge is refused as written since (the import
    // above), and a fresh fixture shows a clean undo after an abandon.
    std::fs::rename(&away, &snap).unwrap();
    let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
    assert_eq!(o.json()["refusal"], "changed_since", "{}", o.out);

    let f = fixture("m22c", false);
    let s = &f.s;
    let before = handles_of(s);
    let o = merge(s, "a", "b", &[]);
    let snap = PathBuf::from(o.json()["snapshot"]["dir"].as_str().unwrap());
    let _ = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", "file:4")]);
    std::fs::rename(&snap, s.0.join("moved")).unwrap();
    assert_eq!(s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-undo"], &[]).rc, 0);
    std::fs::rename(s.0.join("moved"), &snap).unwrap();
    let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(handles_of(s), before, "undone after the abandon");

    // M1, cut at `held`.
    let f = fixture("m22m", false);
    let s = &f.s;
    let o = merge(s, "a", "b", &[]);
    let snap = PathBuf::from(o.json()["snapshot"]["dir"].as_str().unwrap());
    let _ = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", "held")]);
    std::fs::rename(&snap, s.0.join("moved")).unwrap();
    let o = s.import_with("b", &task("x"), &["--remint"]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("was DROPPED before it moved anything") && o.err.contains("is not there any more") && o.err.contains("run it again"), "{}", o.err);
    assert!(!o.err.contains("not merge MG-1's") && !o.err.contains("/files/") && !o.err.contains("cauldron.toml"), "{}", o.err);
    assert_eq!(s.import_with("b", &task("y"), &["--remint"]).rc, 0);
    assert_eq!(s.run(&["merge", "--status", "MG-1", "--into", "b", "--env", "dev"], &[]).json()["state"], "merged");
}

// ------------------------------------------------------------------ M23

/// **What a merge costs is counted in full and refused up front** (M2): a
/// source whose write would spend more than the soup's per-statement fuel
/// (edges cost the most) cannot land as one statement, so it is refused,
/// dry run included and `--allow-large` or not, before anything is held or
/// written, with the size and the fuel in the answer; one estimated to hold
/// the target too long (edges and dump bytes count, not only records) is
/// refused by the same rule as one over the record ceiling, and runs with
/// `--allow-large`. Neither refusal leaves anything behind: the same source
/// merges afterwards. (The budgets are lowered through the fault build's
/// `CAULDRON_MERGE_BUDGET`, so the small fixture reaches them.)
#[cfg(feature = "fault-injection")]
#[test]
fn m23_a_merge_over_the_soup_s_fuel_or_time_is_refused_up_front_and_poisons_nothing() {
    let f = fixture("m23", false);
    let s = &f.s;
    let before = unlocked(&s.cauldron_dir("b"));
    let src_before = unlocked(&s.cauldron_dir("a"));
    for extra in [&["--dry-run"][..], &[][..], &["--allow-large"][..], &["--allow-large", "--dry-run"][..]] {
        let mut a = vec!["merge", "a", "--into", "b", "--env", "dev"];
        a.extend_from_slice(extra);
        let o = s.run_with(CAULDRON, &a, &[], &[("CAULDRON_MERGE_BUDGET", "fuel=40")]);
        assert_eq!(o.rc, 1, "{extra:?}: {}{}", o.out, o.err);
        let j = o.json();
        assert_eq!(j["refusal"], "too_large", "{j}");
        assert!(j["why"].as_str().unwrap().contains("cannot be merged as one write") && j["why"].as_str().unwrap().contains("--allow-large does not change that"), "{j}");
        let size = &j["detail"]["size"];
        assert_eq!(size["fuel"]["over"], true, "{size}");
        assert!(size["edges"].as_u64().unwrap() > 0 && size["fuel"]["estimated"].as_u64().unwrap() > 40, "{size}");
    }
    let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev", "--dry-run"], &[], &[("CAULDRON_MERGE_BUDGET", "seconds=0")]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    let j = o.json();
    assert_eq!(j["refusal"], "too_large", "{j}");
    assert!(j["why"].as_str().unwrap().contains("estimated to take longer") && j["why"].as_str().unwrap().contains("--allow-large"), "{j}");
    assert_eq!(j["detail"]["size"]["over"], json!({"records": false, "time": true, "fuel": false}), "{j}");
    assert_eq!(unlocked(&s.cauldron_dir("b")), before, "a refusal writes nothing to the target");
    assert_eq!(unlocked(&s.cauldron_dir("a")), src_before, "nor to the source");
    let o = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev", "--dry-run", "--allow-large"], &[], &[("CAULDRON_MERGE_BUDGET", "seconds=0")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert!(o.err.contains("estimated to take longer") && o.err.contains("--allow-large"), "{}", o.err);
    // Nothing left behind: the same source merges, as one write.
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let j = o.json();
    assert_eq!(j["size"]["fuel"]["over"], false, "{j}");
    assert!(j["size"]["estimate"]["seconds"].as_u64().is_some() && j["size"]["edges"].as_u64().is_some(), "{j}");
}

// ------------------------------------------------------------------ M24

/// **A write during a merge is refused at once, with when to try again**
/// (M3). Through the `mcp` service: a session's soup_put (and dump) to the
/// target while a merge lands through the service answers in well under a
/// second with `busy` and `retry_after_secs`, instead of waiting in the
/// writer's queue for the length of the merge; reads go on; once the merge
/// is done the same write lands. Directly: an import during a merge the
/// CLI runs is refused at once, `locked`, with `retry_after_secs` and the
/// merge named. (The merge is held open by the fault build's
/// `CAULDRON_MERGE_DELAY_MS`.)
#[cfg(feature = "fault-injection")]
#[test]
fn m24_a_write_during_a_merge_is_refused_at_once_with_a_retry_after() {
    let f = fixture("m24", false);
    let s = &f.s;
    s.grant("b");
    let svc = Service::start_with(s, &service_bin(), &[("CAULDRON_MERGE_DELAY_MS", "6000")]);
    let pids = [svc.pid()];
    let put = |h: &str| json!({"cauldron": "b", "remint": true, "records": [{"id": "t", "type": "task", "state": "ready", "handle": h, "edges": [{"kind": "in_region", "to": "R-1"}]}]});
    // The service opens (and holds) the target with its first write.
    let w = svc.call(s, "soup_put", put("Before the merge"));
    assert_eq!(w["isError"], json!(false), "{w}");
    std::thread::scope(|sc| {
        let m = sc.spawn(|| s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &pids, None));
        wait_landing(s, "b");
        let t0 = Instant::now();
        let w = svc.call(s, "soup_put", put("During the merge"));
        let took = t0.elapsed();
        assert!(took < Duration::from_secs(2), "the write waited {took:?}: {w}");
        assert_eq!(w["isError"], json!(true), "{w}");
        let r = &w["structuredContent"]["refused"];
        assert_eq!(r["code"], "busy", "{w}");
        assert!(r["detail"]["retry_after_secs"].as_u64().is_some_and(|n| n >= 1), "{w}");
        assert!(r["message"].as_str().unwrap().contains("refused at once"), "{w}");
        let d = svc.call(s, "soup_dump", json!({"cauldron": "b", "text": "a note during the merge"}));
        assert_eq!(d["structuredContent"]["refused"]["code"], "busy", "{d}");
        let t0 = Instant::now();
        let g = svc.call(s, "soup_nodes", json!({"cauldron": "b", "limit": 3}));
        assert!(t0.elapsed() < Duration::from_secs(2) && g["isError"] == json!(false), "a read during the merge: {g}");
        let o = m.join().unwrap();
        assert_eq!(o.rc, 0, "{}", o.out);
        assert!(o.out.contains("\"forwarded_to\""), "the merge went through the service: {}", o.out);
    });
    let w = svc.call(s, "soup_put", put("After the merge"));
    assert_eq!(w["isError"], json!(false), "{w}");
    svc.stop();
    s.revoke("b");
    // Directly.
    assert_eq!(s.run(&["create", "c", "--env", "dev"], &[]).rc, 0);
    std::thread::scope(|sc| {
        let m = sc.spawn(|| s.run_with(CAULDRON, &["merge", "a", "--into", "c", "--env", "dev"], &[], &[("CAULDRON_MERGE_DELAY_MS", "5000")]));
        wait_landing(s, "c");
        let t0 = Instant::now();
        let o = s.import_with("c", &task("during"), &["--remint"]);
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
        let j: Value = serde_json::from_str(&o.out).unwrap_or_else(|_| panic!("{}{}", o.out, o.err));
        assert!(j["retry_after_secs"].as_u64().is_some_and(|n| n >= 1) && j["merge_landing"] == "MG-1", "{j}");
        assert!(o.err.contains("merge MG-1 is landing") && o.err.contains("retry_after_secs"), "{}", o.err);
        assert_eq!(m.join().unwrap().rc, 0);
    });
    assert_eq!(s.import_with("c", &task("after"), &["--remint"]).rc, 0);
}

/// Wait until merge MG-1 into `c` is landing (its journal written, the
/// target held).
fn wait_landing(s: &Scratch, c: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !std::fs::read_to_string(s.cauldron_dir(c).join("merges/MG-1/journal.json")).is_ok_and(|t| t.contains("\"landing\"")) {
        assert!(Instant::now() < deadline, "merge MG-1 into {c} never began landing");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
}

// ------------------------------------------------------------------ M25

/// **The lower findings, closed.** L1: undoing the owner's merge is the
/// owner act `merge` (an agent is refused unless it is opened). L5: a
/// renamed cauldron still undoes, keeps its new name, its status names it,
/// and a new cauldron under the old name does not list the snapshot as
/// unused. L6: an empty source is refused, writing nothing. L7: the
/// refusal after a later write says how to see those writes and why
/// keeping them is not offered, and names a directory that is there.
#[test]
fn m25_undo_is_the_owner_act_a_renamed_cauldron_undoes_an_empty_source_is_refused() {
    // L1.
    let f = fixture("m25a", true);
    let s = &f.s;
    let o = s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
    assert_eq!(o.rc, 1, "an agent undid the owner's merge: {}", o.out);
    assert_eq!(o.json()["refusal"], "not_a_terminal", "{}", o.out);
    assert!(o.err.contains("the same owner act `merge`") && o.err.contains("owner-acts open"), "{}", o.err);
    let o = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_OWNER_ACTS", "merge")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    // L5.
    let f = fixture("m25r", false);
    let s = &f.s;
    let before = handles_of(s);
    assert_eq!(merge(s, "a", "b", &[]).rc, 0);
    let o = s.run(&["rename", "b", "b2", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let st = s.run(&["merge", "--status", "MG-1", "--into", "b2", "--env", "dev"], &[]).json();
    assert_eq!(st["undo_command"], "cauldron merge --undo MG-1 --into b2 --env dev", "{st}");
    assert_eq!(st["snapshots"].as_array().map(Vec::len), Some(1), "{st}");
    let o = s.run(&["merge", "--undo", "MG-1", "--into", "b2", "--env", "dev"], &[]);
    assert_eq!(o.rc, 0, "a renamed cauldron undoes: {}{}", o.out, o.err);
    let toml = std::fs::read_to_string(s.cauldron_dir("b2").join("cauldron.toml")).unwrap();
    assert!(toml.contains("\"b2\""), "the undo kept the new name: {toml}");
    let mut now: Vec<String> = s.nodes("b2").iter().map(|n| n["handle"].as_str().unwrap_or("").to_string()).collect();
    now.sort();
    assert_eq!(now, before);
    assert_eq!(s.run(&["merge", "--status", "MG-1", "--into", "b2", "--env", "dev"], &[]).json()["state"], "undone");
    assert_eq!(s.run(&["create", "b", "--env", "dev"], &[]).rc, 0);
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b"], &[]);
    assert!(!o.out.contains("b.MG-1-before-"), "another cauldron's snapshot listed as unused: {}", o.out);
    // L6.
    assert_eq!(s.run(&["create", "empty", "--env", "dev"], &[]).rc, 0);
    let b2 = unlocked(&s.cauldron_dir("b2"));
    let o = merge(s, "empty", "b2", &[]);
    assert_eq!(o.rc, 1, "{}{}", o.out, o.err);
    assert_eq!(o.json()["refusal"], "already_merged", "{}", o.out);
    assert!(o.out.contains("holds no record and no dump"), "{}", o.out);
    assert_eq!(unlocked(&s.cauldron_dir("b2")), b2, "an empty source wrote to the target");
    // L7.
    let f = fixture("m25w", false);
    let s = &f.s;
    let o = merge(s, "a", "b", &[]);
    let snap = PathBuf::from(o.json()["snapshot"]["dir"].as_str().unwrap());
    let o = s.import_with("b", &task("Written after the merge"), &["--remint"]);
    assert_eq!(o.rc, 0);
    let o = s.run(&["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[]);
    assert_eq!(o.json()["refusal"], "changed_since", "{}", o.out);
    let why = o.json()["why"].as_str().unwrap().to_string();
    assert!(why.contains("cauldron soup get ") && why.contains("supersedes") && why.contains("another transaction"), "{why}");
    assert!(why.contains(&snap.join("files").display().to_string()) && snap.join("files").is_dir(), "{why}");
}

/// **L4: `cauldron merge` finishes an undo cut short** before it looks at
/// the target, as every writer of this release does, then merges.
#[cfg(feature = "fault-injection")]
#[test]
fn m26_a_merge_finishes_an_undo_cut_short_first() {
    let f = fixture("m26", false);
    let s = &f.s;
    assert_eq!(merge(s, "a", "b", &[]).rc, 0);
    let o = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", "file:3")]);
    assert_ne!(o.rc, 0);
    let o = merge(s, "a", "b", &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let st = s.run(&["merge", "--status", "MG-1", "--into", "b", "--env", "dev"], &[]).json();
    assert_eq!(st["state"], "merged", "{st}");
}

// ------------------------------------------------------------------ M27

/// **`--abandon-undo` ends exactly as merged, and is the owner's act**
/// (B39i H1, M1). An undo cut after the dumps store was put back (file:4)
/// or after both stores were (file:7), its snapshot then moved away:
/// `--abandon-undo` returns every part, so the records, the dumps and
/// `--status` all say merged, no record cites a missing dump, and the next
/// dump does not take a cited id (it took `DM-3` before). On the owner's
/// merge, an agent is refused (`not_a_terminal`); with `merge` opened, an
/// undo that can still be finished is refused naming `--lose-undo`, which
/// then keeps the merge. `--abandon-merge` refuses a merge recover can
/// still settle unless `--keep-partial-merge`.
#[cfg(feature = "fault-injection")]
#[test]
fn m27_an_abandoned_undo_ends_exactly_as_merged_and_is_the_owner_s_act() {
    for step in ["file:4", "file:7"] {
        let f = fixture(&format!("m27-{}", step.replace(':', "")), false);
        let s = &f.s;
        let o = merge(s, "a", "b", &[]);
        let snap = PathBuf::from(o.json()["snapshot"]["dir"].as_str().unwrap());
        let (merged, merged_dumps) = (handles_of(s), s.dumps("b").len());
        let o = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", step)]);
        assert_ne!(o.rc, 0);
        std::fs::rename(&snap, s.0.join("away")).unwrap();
        let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-undo"], &[]);
        assert_eq!(o.rc, 0, "{step}: {}{}", o.out, o.err);
        let u = printed_json(&o.out)["merge_undo"].clone();
        assert_eq!(u["outcome"], "undo-abandoned", "{step}: {u}");
        assert_eq!(u["end_state"]["state"], "merged", "{step}: {u}");
        assert_eq!((handles_of(s), s.dumps("b").len()), (merged.clone(), merged_dumps), "{step}");
        assert_eq!(s.run(&["merge", "--status", "MG-1", "--into", "b", "--env", "dev"], &[]).json()["state"], "merged", "{step}");
        let cited: Vec<String> = s.nodes("b").iter().flat_map(|n| s.get("b", n["id"].as_str().unwrap())["records"][0]["meta"]["cites"].as_array().cloned().unwrap_or_default()).filter_map(|c| c["id"].as_str().map(str::to_string)).collect();
        let ids: Vec<String> = s.dumps("b").iter().filter_map(|d| d["id"].as_str().map(str::to_string)).collect();
        assert!(cited.iter().all(|c| ids.contains(c)), "{step}: a record cites a missing dump: {cited:?} {ids:?}");
        let fresh = s.dump("b", "a note after the abandon");
        assert!(!cited.contains(&fresh), "{step}: the new dump took a cited id {fresh}");
    }
    // The owner's merge: the abandon is the owner act.
    let f = fixture("m27o", true);
    let s = &f.s;
    let o = s.on_tty(&["merge", "a", "--into", "b", "--env", "dev"], &[], None);
    assert_eq!(o.rc, 0, "{}", o.out);
    let o = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", "file:2"), ("CAULDRON_OWNER_ACTS", "merge")]);
    assert_ne!(o.rc, 0);
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-undo"], &[]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("same owner act `merge`"), "{}", o.err);
    let o = s.run_with(CAULDRON, &["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-undo"], &[], &[("CAULDRON_OWNER_ACTS", "merge")]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("can still be finished") && o.err.contains("--lose-undo"), "{}", o.err);
    let o = s.run_with(CAULDRON, &["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-undo", "--lose-undo"], &[], &[("CAULDRON_OWNER_ACTS", "merge")]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    assert_eq!(printed_json(&o.out)["merge_undo"]["end_state"]["state"], "merged", "{}", o.out);
    // A merge recover can settle: --abandon-merge needs --keep-partial-merge.
    let f = fixture("m27m", false);
    let s = &f.s;
    let _ = s.run_with(CAULDRON, &["merge", "a", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", "dumps")]);
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-merge"], &[]);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("can still be settled") && o.err.contains("--keep-partial-merge"), "{}", o.err);
    let o = s.run(&["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-merge", "--keep-partial-merge"], &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let r = printed_json(&o.out);
    assert_eq!(r["outcome"], "merge-abandoned", "{r}");
    assert_eq!(s.import_with("b", &task("after the abandon"), &["--remint"]).rc, 0);
}

// ------------------------------------------------------------------ M28

/// A copy of `t`'s whole tree (both cauldrons, their merges and
/// snapshots), a scratch of its own: one setup, many cells.
#[cfg(feature = "fault-injection")]
fn clone_of(t: &Scratch, tag: &str) -> Scratch {
    let p = std::env::temp_dir().join(format!(
        "cauldron-merge-it-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let st = Command::new("/bin/cp").arg("-c").arg("-R").arg(&t.0).arg(&p).status().unwrap();
    assert!(st.success(), "cp -c -R {} {}", t.0.display(), p.display());
    Scratch(p.canonicalize().unwrap())
}

#[cfg(feature = "fault-injection")]
fn handles_in(s: &Scratch, c: &str) -> Vec<String> {
    let mut v: Vec<String> = s.nodes(c).iter().map(|n| n["handle"].as_str().unwrap_or("").to_string()).collect();
    v.retain(|h| !h.starts_with("m28 probe"));
    v.sort();
    v
}

/// Every dump id a record of `c` cites.
#[cfg(feature = "fault-injection")]
fn cited_in(s: &Scratch, c: &str) -> Vec<String> {
    let ids: Vec<String> = s.nodes(c).iter().filter_map(|n| n["id"].as_str().map(str::to_string)).collect();
    if ids.is_empty() {
        return vec![];
    }
    let mut a = vec!["soup", "get"];
    a.extend(ids.iter().map(String::as_str));
    a.extend(["--env", "dev", "--cauldron", c]);
    let o = s.run(&a, &[]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    o.json()["records"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["meta"]["cites"].as_array().cloned().unwrap_or_default())
        .filter(|c| c["kind"] == "dump")
        .filter_map(|c| c["id"].as_str().map(str::to_string))
        .collect()
}

#[cfg(feature = "fault-injection")]
fn status_of(s: &Scratch, c: &str, id: &str) -> Value {
    let o = s.run(&["merge", "--status", id, "--into", c, "--env", "dev"], &[]);
    serde_json::from_str(&o.out).unwrap_or_else(|_| json!({"state": "none", "out": o.out, "err": o.err}))
}

/// One composition of M28: the template (everything before the merge under
/// test), the merge under test, the merges before it that must stay merged,
/// and what the merge under test carries.
#[cfg(feature = "fault-injection")]
struct Comp {
    name: &'static str,
    t: Scratch,
    from: &'static str,
    into: &'static str,
    id: &'static str,
    prior: Vec<(&'static str, &'static str)>,
    /// `merge --status`'s `stores.carried.what`; `None`: nothing new, refused.
    what: Option<&'static str>,
}

#[cfg(feature = "fault-injection")]
fn comps() -> Vec<Comp> {
    let records_only = |r: Value| -> Value {
        let mut r = r;
        for x in r.as_array_mut().unwrap() {
            if let Some(m) = x.as_object_mut() {
                m.remove("meta");
            }
        }
        r
    };
    let mut v = Vec::new();
    // Records only, a first merge: a source with no dump at all.
    let s = Scratch::new("m28-ro", &["a", "b"]);
    assert_eq!(s.import("a", &records_only(source_records())).rc, 0);
    assert_eq!(s.import("b", &target_records()).rc, 0);
    s.dump("b", ONLY_B);
    v.push(Comp { name: "records only", t: s, from: "a", into: "b", id: "MG-1", prior: vec![], what: Some("records only") });
    // Dumps only, a first merge: a source with no record at all (one of its
    // dumps the target already holds).
    let s = Scratch::new("m28-do", &["a", "b"]);
    s.dump("a", SHARED);
    s.dump("a", ONLY_A);
    assert_eq!(s.import("b", &target_records()).rc, 0);
    s.dump("b", SHARED);
    v.push(Comp { name: "dumps only", t: s, from: "a", into: "b", id: "MG-1", prior: vec![], what: Some("dumps only") });
    // Both (the fixture: records, ripple records, dumps, a tombstone).
    let f = fixture("m28-both", false);
    v.push(Comp { name: "records and dumps", t: f.s, from: "a", into: "b", id: "MG-1", prior: vec![], what: Some("records and dumps") });
    // A second merge between the same pair: records only.
    let f = fixture("m28-2r", false);
    assert_eq!(merge(&f.s, "a", "b", &[]).rc, 0);
    assert_eq!(f.s.import_with("a", &task("A task the source gained after the first merge"), &["--remint"]).rc, 0);
    v.push(Comp { name: "second merge, records only", t: f.s, from: "a", into: "b", id: "MG-2", prior: vec![("b", "MG-1")], what: Some("records only") });
    // A second merge between the same pair: dumps only (the usual one).
    let f = fixture("m28-2d", false);
    assert_eq!(merge(&f.s, "a", "b", &[]).rc, 0);
    f.s.dump("a", "A note the source gained after the first merge.");
    v.push(Comp { name: "second merge, dumps only", t: f.s, from: "a", into: "b", id: "MG-2", prior: vec![("b", "MG-1")], what: Some("dumps only") });
    // A third: after a dumps-only second, a record citing a new dump.
    let f = fixture("m28-3", false);
    assert_eq!(merge(&f.s, "a", "b", &[]).rc, 0);
    f.s.dump("a", "A note the source gained after the first merge.");
    assert_eq!(merge(&f.s, "a", "b", &[]).rc, 0);
    let d = f.s.dump("a", "A note the source gained after the second merge.");
    let mut t = task("A task citing the newest note");
    t[0]["meta"] = json!({"cites": [{"kind": "dump", "id": d}]});
    let o = f.s.import_with("a", &t, &["--remint"]);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    v.push(Comp { name: "third merge, records and dumps", t: f.s, from: "a", into: "b", id: "MG-3", prior: vec![("b", "MG-1"), ("b", "MG-2")], what: Some("records and dumps") });
    // Nothing new: refused, at every cut point.
    let f = fixture("m28-none", false);
    assert_eq!(merge(&f.s, "a", "b", &[]).rc, 0);
    v.push(Comp { name: "nothing new", t: f.s, from: "a", into: "b", id: "MG-2", prior: vec![("b", "MG-1")], what: None });
    // A into B, then B into A: only what is B's own goes back.
    let f = fixture("m28-back", false);
    assert_eq!(merge(&f.s, "a", "b", &[]).rc, 0);
    v.push(Comp { name: "a into b, then b into a", t: f.s, from: "b", into: "a", id: "MG-1", prior: vec![("b", "MG-1")], what: Some("records and dumps") });
    v
}

/// **Where a merge stands is judged from what it carried** (records only,
/// dumps only, both, nothing new), at every cut point. For each composition
/// (a first merge of records only, of dumps only, of both; a second merge
/// of records only and of dumps only, a third of both, a merge with nothing
/// new, and b merged back into a after a into b) the merge is run whole, cut
/// after each of its steps and then recovered, or run whole and its undo
/// cut after each of its steps and then finished by `soup recover` or (its
/// snapshot moved away) given up by `--abandon-undo`. Every cell must end
/// on ONE side, merged or as before the merge, with the records, the dumps,
/// `merge --status` (and its `stores`, naming what was carried), the
/// abandon's answer and `--settle-merge` all agreeing: never `torn` for a
/// merge that is whole, never an empty reason. The merges before it stay
/// merged; no record cites a missing dump; a new dump takes no cited id;
/// the target is writable; a merged cell undoes cleanly, with nothing
/// listed as written by something else. The table is printed.
#[cfg(feature = "fault-injection")]
#[test]
fn m28_where_a_merge_stands_follows_what_it_carried_at_every_cut_point() {
    let merge_cuts = ["snapshot", "staged", "journal", "held", "dump", "dumps", "soup", "ripple", "record"];
    let undo_cuts = ["marked", "held", "recorded", "file:1", "file:2", "file:3", "file:4", "file:5", "file:6", "file:8", "restored"];
    let comps = comps();
    // The two sides of each composition, from a whole merge of a copy.
    let sides: Vec<((Vec<String>, usize), (Vec<String>, usize))> = comps
        .iter()
        .map(|c| {
            let pre = (handles_in(&c.t, c.into), c.t.dumps(c.into).len());
            if c.what.is_none() {
                return (pre.clone(), pre);
            }
            let s = clone_of(&c.t, "m28-ref");
            let o = merge(&s, c.from, c.into, &[]);
            assert_eq!(o.rc, 0, "{}: {}{}", c.name, o.out, o.err);
            assert_eq!(o.json()["merge"], c.id, "{}", c.name);
            (pre, (handles_in(&s, c.into), s.dumps(c.into).len()))
        })
        .collect();
    #[derive(Clone, Copy)]
    enum Cut {
        Whole,
        Merge(&'static str),
        Undo(&'static str, bool),
    }
    let mut cells: Vec<(usize, Cut)> = Vec::new();
    for (i, c) in comps.iter().enumerate() {
        cells.push((i, Cut::Whole));
        cells.extend(merge_cuts.iter().map(|p| (i, Cut::Merge(p))));
        if c.what.is_some() {
            for moved in [false, true] {
                cells.extend(undo_cuts.iter().map(|p| (i, Cut::Undo(p, moved))));
            }
        }
    }
    let cell = |(i, cut): (usize, Cut)| -> (String, bool) {
        let c = &comps[i];
        let (pre, merged) = &sides[i];
        let label = match cut {
            Cut::Whole => "whole".to_string(),
            Cut::Merge(p) => format!("merge cut at {p}"),
            Cut::Undo(p, false) => format!("undo cut at {p}"),
            Cut::Undo(p, true) => format!("undo cut at {p}, snapshot moved"),
        };
        let s = clone_of(&c.t, &format!("m28-{i}"));
        let (from, into, id) = (c.from, c.into, c.id);
        let owner = [("CAULDRON_OWNER_ACTS", "merge")];
        let mut said: Vec<String> = Vec::new();
        let mut bad: Vec<String> = Vec::new();
        let snap_of = |s: &Scratch| -> Option<PathBuf> {
            let mut v: Vec<PathBuf> = std::fs::read_dir(s.data().join("cauldrons"))
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!("{into}.{id}-before-")))
                .collect();
            v.sort();
            v.pop()
        };
        let mut moved_snap = false;
        match cut {
            Cut::Whole => {
                let o = merge(&s, from, into, &[]);
                said.push(format!("merge rc {}", o.rc));
                if c.what.is_some() && o.rc != 0 {
                    bad.push(format!("the merge failed: {}", o.err.lines().next().unwrap_or("")));
                }
                if c.what.is_none() && o.rc == 0 {
                    bad.push("a merge with nothing new landed".into());
                }
            }
            Cut::Merge(p) => {
                let o = s.run_with(CAULDRON, &["merge", from, "--into", into, "--env", "dev"], &[], &[("CAULDRON_MERGE_FAULT", p)]);
                if o.rc == 0 {
                    said.push("ran through (no such step for this merge)".into());
                    if c.what.is_none() {
                        bad.push("a merge with nothing new landed".into());
                    }
                } else if c.what.is_none() {
                    // Nothing new: refused before anything, whatever the cut.
                    let refused = serde_json::from_str::<Value>(&o.out).ok().and_then(|v| v["refusal"].as_str().map(str::to_string));
                    said.push(format!("refused ({})", refused.clone().unwrap_or_else(|| "aborted".into())));
                    if refused.is_none() {
                        bad.push("a merge with nothing new did not refuse".into());
                    }
                } else {
                    let r = s.run(&["soup", "recover", "--env", "dev", "--cauldron", into], &[]);
                    said.push(format!("cut; recover rc {} ({})", r.rc, serde_json::from_str::<Value>(&printed_json_or(&r.out)).ok().and_then(|v| v["outcome"].as_str().map(str::to_string)).unwrap_or_default()));
                    if r.rc != 0 {
                        bad.push(format!("recover refused: {}", r.err.lines().next().unwrap_or("")));
                    }
                }
            }
            Cut::Undo(p, moved) => {
                let o = merge(&s, from, into, &[]);
                assert_eq!(o.rc, 0, "{}: {}", c.name, o.err);
                let o = s.run_with(CAULDRON, &["merge", "--undo", id, "--into", into, "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", p)]);
                if o.rc == 0 {
                    said.push("undo ran through (no such step)".into());
                } else if moved {
                    if let Some(sn) = snap_of(&s) {
                        std::fs::rename(&sn, s.0.join("snapshot-away")).unwrap();
                        moved_snap = true;
                    }
                    let r = s.run_with(CAULDRON, &["soup", "recover", "--env", "dev", "--cauldron", into, "--abandon-undo"], &[], &owner);
                    let u = serde_json::from_str::<Value>(&printed_json_or(&r.out)).unwrap_or(Value::Null);
                    let u = if u["merge_undo"].is_object() { u["merge_undo"].clone() } else { u };
                    said.push(format!("cut, snapshot moved; abandon-undo rc {} ({})", r.rc, u["outcome"].as_str().unwrap_or("")));
                    if r.rc != 0 {
                        // Refused: only if it names a way that works.
                        let r2 = s.run(&["soup", "recover", "--env", "dev", "--cauldron", into], &[]);
                        said.push(format!("then recover rc {}", r2.rc));
                        if r2.rc != 0 {
                            bad.push(format!("abandon-undo refused: {}", r.err.lines().next().unwrap_or("")));
                        }
                    } else {
                        if u["outcome"] != "undo-abandoned" && u["outcome"] != "none" {
                            bad.push(format!("the abandon said {}: {}", u["outcome"], u["note"]));
                        }
                        if u["note"].as_str().is_some_and(|n| n.contains("is torn: ;") || n.contains(": ;")) {
                            bad.push("an empty reason".into());
                        }
                    }
                } else {
                    let r = s.run(&["soup", "recover", "--env", "dev", "--cauldron", into], &[]);
                    said.push(format!("cut; recover rc {}", r.rc));
                    if r.rc != 0 {
                        bad.push(format!("recover refused: {}", r.err.lines().next().unwrap_or("")));
                    }
                }
            }
        }
        // The side the stores are on.
        let now = (handles_in(&s, into), s.dumps(into).len());
        let side = if c.what.is_none() {
            if &now == pre { "unchanged" } else { "CHANGED" }
        } else if &now == merged {
            "merged"
        } else if &now == pre {
            "before"
        } else {
            "MIXED"
        };
        if side == "MIXED" || side == "CHANGED" {
            bad.push(format!("the stores are on neither side: {now:?}"));
        }
        let st = status_of(&s, into, id);
        let state = st["state"].as_str().unwrap_or("none").to_string();
        let stores = st["stores"]["state"].as_str().unwrap_or("-").to_string();
        let carried = st["stores"]["carried"]["what"].as_str().unwrap_or("-").to_string();
        match side {
            "merged" => {
                if state != "merged" || stores != "merged" {
                    bad.push(format!("merged, but --status says {state} (stores {stores}): {}", st["note"]));
                }
                if Some(carried.as_str()) != c.what {
                    bad.push(format!("carried {carried}, not {:?}", c.what));
                }
            }
            "before" => {
                if ["merged", "torn", "partial"].contains(&state.as_str()) || (stores != "-" && stores != "before-merge") {
                    bad.push(format!("before the merge, but --status says {state} (stores {stores})"));
                }
            }
            _ => {
                if state != "none" {
                    bad.push(format!("nothing new, but --status {id} says {state}"));
                }
            }
        }
        // The settle agrees: nothing to settle on either side.
        if c.what.is_some() {
            let r = s.run_with(CAULDRON, &["soup", "recover", "--env", "dev", "--cauldron", into, "--settle-merge", id], &[], &owner);
            let v = serde_json::from_str::<Value>(&printed_json_or(&r.out)).unwrap_or(Value::Null);
            if r.rc != 0 || v["outcome"] != "nothing-to-settle" {
                bad.push(format!("settle rc {}: {}", r.rc, r.err.lines().next().unwrap_or("")));
            }
        }
        // The merges before it stay merged.
        for (pc, pid) in &c.prior {
            let p = status_of(&s, pc, pid);
            if p["state"] != "merged" {
                bad.push(format!("{pid} into {pc} is now {}", p["state"]));
            }
        }
        // No cite of a missing dump; a new dump takes no cited id.
        let cited = cited_in(&s, into);
        let ids: Vec<String> = s.dumps(into).iter().filter_map(|d| d["id"].as_str().map(str::to_string)).collect();
        let missing: Vec<&String> = cited.iter().filter(|x| !ids.contains(x)).collect();
        if !missing.is_empty() {
            bad.push(format!("cites of missing dumps: {missing:?}"));
        }
        let fresh = s.dump(into, &format!("m28 note after the cell {label}"));
        if cited.contains(&fresh) {
            bad.push(format!("the new dump took a cited id {fresh}"));
        }
        // Writable.
        let o = s.import_with(into, &task("m28 probe"), &["--remint"]);
        if o.rc != 0 {
            bad.push(format!("not writable: {}", o.err.lines().next().unwrap_or("")));
        }
        // A merged cell undoes cleanly (the later dump and import are
        // writes since, so a fresh copy of the cell's merge is undone).
        let mut tail = String::new();
        if side == "merged" && !moved_snap && matches!(cut, Cut::Whole | Cut::Merge(_)) {
            let s2 = clone_of(&c.t, &format!("m28-{i}-undo"));
            let o = merge(&s2, from, into, &[]);
            if o.rc == 0 {
                let u = s2.run(&["merge", "--undo", id, "--into", into, "--env", "dev"], &[]);
                let v = serde_json::from_str::<Value>(&printed_json_or(&u.out)).unwrap_or(Value::Null);
                let kept = v["restored"]["kept"].as_array().map(|a| a.len()).unwrap_or(0);
                let back = (handles_in(&s2, into), s2.dumps(into).len());
                tail = format!("; undo rc {}, kept {kept}", u.rc);
                if u.rc != 0 || &back != pre || kept != 0 || status_of(&s2, into, id)["state"] != "undone" {
                    bad.push(format!("the undo: rc {} kept {kept} back {} {}", u.rc, &back == pre, u.err.lines().next().unwrap_or("")));
                }
            }
        }
        let ok = bad.is_empty();
        (
            format!(
                "| {} | {label} | {} | {side} | {state} / {stores} / {carried}{tail} | {} |",
                c.name,
                said.join("; "),
                if ok { "consistent".to_string() } else { format!("NO: {}", bad.join("; ")) }
            ),
            ok,
        )
    };
    let rows: Vec<(String, bool)> = std::thread::scope(|sc| {
        let cell = &cell;
        cells
            .chunks(cells.len().div_ceil(8))
            .map(|chunk| sc.spawn(move || chunk.iter().map(|c| cell(*c)).collect::<Vec<_>>()))
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    eprintln!("| composition | cut | what happened | side | status / stores / carried | verdict |\n|---|---|---|---|---|---|");
    for (r, _) in &rows {
        eprintln!("{r}");
    }
    let bad: Vec<&String> = rows.iter().filter(|r| !r.1).map(|r| &r.0).collect();
    eprintln!("m28: {} cells, {} consistent", rows.len(), rows.len() - bad.len());
    assert!(bad.is_empty(), "{} cell(s) failed:\n{}", bad.len(), bad.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
}

/// The JSON a command printed (after any words), as text; `{}` if none.
#[cfg(feature = "fault-injection")]
fn printed_json_or(text: &str) -> String {
    match (text.find("{\n"), text.rfind("\n}")) {
        (Some(a), Some(b)) if b > a => text[a..b + 2].to_string(),
        _ => text.trim().to_string(),
    }
}

// ------------------------------------------------------------------ M29

/// **A torn cauldron says what the owner can do, and it works** (the double
/// deletion by hand: an undo cut after it put files back, then both its
/// snapshot and the copies it moved aside taken away). `--abandon-undo`,
/// `merge --status` and `--settle-merge` name each record that cites a
/// missing dump and say that merging the source again does not repair it
/// (true: it is checked on a copy); following the commands the message
/// gives (copy the note out of the source, dump it, re-point the cite) and
/// then `--settle-merge` ends the cauldron no longer torn, its record
/// naming what was lost, every cite resolving, no id reused.
#[cfg(feature = "fault-injection")]
#[test]
fn m29_a_torn_cauldron_says_what_can_be_done_and_doing_it_settles_it() {
    let f = fixture("m29", false);
    let s = &f.s;
    let owner = [("CAULDRON_OWNER_ACTS", "merge")];
    assert_eq!(merge(s, "a", "b", &[]).rc, 0);
    let o = s.run_with(CAULDRON, &["merge", "--undo", "MG-1", "--into", "b", "--env", "dev"], &[], &[("CAULDRON_UNDO_FAULT", "file:3")]);
    assert_ne!(o.rc, 0);
    let away = s.0.join("away");
    std::fs::create_dir_all(&away).unwrap();
    for infix in ["-before-", "-undone-"] {
        let p = beside(s, "b", infix).unwrap();
        std::fs::rename(&p, away.join(p.file_name().unwrap())).unwrap();
    }
    let o = s.run_with(CAULDRON, &["soup", "recover", "--env", "dev", "--cauldron", "b", "--abandon-undo"], &[], &owner);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let u = printed_json(&o.out)["merge_undo"].clone();
    assert_eq!(u["outcome"], "undo-abandoned-torn", "{u}");
    let note = u["note"].as_str().unwrap();
    assert!(!note.contains(": ;") && note.contains("Merging the source again does NOT repair") && note.contains("cauldron soup cite"), "{note}");
    let missing = u["end_state"]["missing_cites"].as_array().cloned().unwrap_or_default();
    assert!(!missing.is_empty(), "{u}");
    let st = status_of(s, "b", "MG-1");
    assert_eq!(st["state"], "torn", "{st}");
    assert!(st["note"].as_str().unwrap().contains("What you can do"), "{st}");
    let o = s.run_with(CAULDRON, &["soup", "recover", "--env", "dev", "--cauldron", "b", "--settle-merge", "MG-1"], &[], &owner);
    assert_ne!(o.rc, 0);
    assert!(o.err.contains("What you can do") && !o.err.contains("The source cauldron still holds") && !o.err.contains("the undo's snapshot, or the copies"), "{}", o.err);
    // The message is right that merging again does not repair it.
    let c = clone_of(s, "m29-remerge");
    let _ = merge(&c, "a", "b", &[]);
    let ids: Vec<String> = c.dumps("b").iter().filter_map(|d| d["id"].as_str().map(str::to_string)).collect();
    for m in &missing {
        assert!(!ids.contains(&m["cites"].as_str().unwrap().to_string()), "a merge again brought back {m}");
        assert!(cited_in(&c, "b").contains(&m["cites"].as_str().unwrap().to_string()), "{m}");
    }
    drop(c);
    // Doing what it says: the cites that can be changed re-pointed (a
    // ripple's record's dump cite is its review trail: never changed, and
    // refused if tried).
    assert!(missing.iter().any(|m| m["can_be_changed"] == json!(true)), "{missing:?}");
    for m in missing.iter().filter(|m| m["can_be_changed"] == json!(false)) {
        let rm = json!({"kind": "dump", "id": m["cites"]}).to_string();
        let o = s.run(&["soup", "cite", m["record"].as_str().unwrap(), "--remove", &rm, "--env", "dev", "--cauldron", "b"], &[]);
        assert!(o.rc != 0 && o.err.contains("dump-cite-is-provenance"), "{}", o.err);
        assert!(note.contains(&format!("{} cites {}", m["record"].as_str().unwrap(), m["cites"].as_str().unwrap())) && note.contains("never changed"), "{note}");
    }
    for m in missing.iter().filter(|m| m["can_be_changed"] == json!(true)) {
        let (rec, gone, src) = (m["record"].as_str().unwrap(), m["cites"].as_str().unwrap(), m["source_dump"].as_str().unwrap());
        let o = s.run(&["soup", "dump-get", src, "--raw", "--env", "dev", "--cauldron", "a"], &[]);
        assert_eq!(o.rc, 0, "{}", o.err);
        let new = s.dump("b", &o.out);
        let rm = json!({"kind": "dump", "id": gone}).to_string();
        let add = json!({"kind": "dump", "id": new}).to_string();
        for (flag, v) in [("--remove", &rm), ("--add", &add)] {
            let o = s.run(&["soup", "cite", rec, flag, v, "--env", "dev", "--cauldron", "b"], &[]);
            assert_eq!(o.rc, 0, "{flag}: {}{}", o.out, o.err);
        }
    }
    let o = s.run_with(CAULDRON, &["soup", "recover", "--env", "dev", "--cauldron", "b", "--settle-merge", "MG-1"], &[], &owner);
    assert_eq!(o.rc, 0, "{}{}", o.out, o.err);
    let r = printed_json(&o.out)["merge"].clone();
    assert_eq!(r["outcome"], "settled-as-it-stands", "{r}");
    assert!(!r["lost"]["dumps"].as_object().unwrap().is_empty(), "{r}");
    let st = status_of(s, "b", "MG-1");
    assert_eq!(st["state"], "merged", "{st}");
    let left: Vec<String> = r["lost"]["cites_left_missing"].as_array().unwrap().iter().map(|x| x["cites"].as_str().unwrap().to_string()).collect();
    let trail: Vec<String> = missing.iter().filter(|m| m["can_be_changed"] == json!(false)).map(|m| m["cites"].as_str().unwrap().to_string()).collect();
    assert_eq!(left, trail, "{r}");
    assert_eq!(st["stores"]["settled"]["lost"], r["lost"], "{st}");
    let cited = cited_in(s, "b");
    let ids: Vec<String> = s.dumps("b").iter().filter_map(|d| d["id"].as_str().map(str::to_string)).collect();
    assert!(cited.iter().all(|x| ids.contains(x) || left.contains(x)), "{cited:?} {ids:?}");
    let fresh = s.dump("b", "a note after the settle");
    assert!(!cited.contains(&fresh) && !missing.iter().any(|m| m["cites"] == json!(fresh)), "{fresh}");
    assert_eq!(s.import_with("b", &task("after the settle"), &["--remint"]).rc, 0);
}
