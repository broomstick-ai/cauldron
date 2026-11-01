//! **RV1.S12a: `cauldron ctl dump | frame | node`** — the coordinator's face on
//! the TUI's own calls (EPIC-2 M0).
//!
//! The router execs the face found BESIDE its own binary, so every test
//! installs the built `cauldron` and the built `tui` side by side in a scratch
//! `bin/` (as `face_env.rs` does with a fake) and runs them under a scratch
//! `CAULDRON_HOME`/`HOME`. This binary exports `CAULDRON_ISOLATED=1`
//! (`test_isolation::install!`), so every child is isolated.
//!
//! **The `tui` here is the REAL one**, which `cargo test -p cauldron` does not
//! build: it is looked for beside this crate's `cauldron`, then in the
//! workspace's `target/debug/`. Under cargo-lease `target/debug/` holds only
//! the CURRENT command's outputs, so run this with the face in the same
//! command: `cargo test -p cauldron -p tui --test it ctl`. A missing or
//! stale one — one that does not know `--send` — FAILS the test rather than
//! skipping it.
//!
//! - **C1** `ctl dump` and the face's own `--dump --send`, given the same
//!   bucket, append the same record (`seq`/`at` masked), and what `ctl`
//!   printed is the line it appended. Opposed: one extra `--quote` on the ctl
//!   side differs. (That `--send` and the send KEY are one function is proved
//!   where the key can be pressed: `apps/tui/tests/it/ctl_send.rs`.)
//! - **C2** `ctl dump` into `cauldron-build` is refused and appends nothing;
//!   opposed by C1's fresh environment, which gains exactly one line.
//! - **C3** `ctl node` reads one node through a scratch soup door at the
//!   address the seam table declares, and passes the door's refusal code
//!   through verbatim; an id with no row exits 1 with `not_found`.
//! - **C4** `ctl frame --status-tab ledger` shows the new dump at the top,
//!   asserted on the frame it printed; opposed by the frame before any dump.
//! - **C5** a seam row pointing off loopback is refused by the egress rule
//!   before anything is dialled (REL-6a6's rule: MERGE-V1j / REL-6a8 is not in
//!   this base).

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

const CAULDRON: &str = env!("CARGO_BIN_EXE_cauldron");

/// The built face, or a failure saying how to build it. Fresh means it knows
/// `--send` (its usage names it): a stale `tui` would pass C1 for nothing.
fn tui_bin() -> PathBuf {
    let beside = Path::new(CAULDRON).parent().map(|d| d.join("tui"));
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join("debug").join("tui");
    let found = beside.into_iter().chain([ws]).find(|p| p.is_file()).unwrap_or_else(|| {
        panic!("no built `tui` beside {CAULDRON} or in target/debug: run it in the same cargo command as the face (`cargo test -p cauldron -p tui --test it ctl`): cargo-lease leaves only the CURRENT command's outputs in target/debug")
    });
    let help = Command::new(&found).args(["--cauldron", "--help"]).env("CAULDRON_OPENER", "none").output().expect("run tui --help");
    assert!(String::from_utf8_lossy(&help.stdout).contains("--send"), "{} is stale (no --send): rebuild with `cargo build --workspace`", found.display());
    found
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A scratch dir holding `bin/{cauldron,tui}` (hard links where the volume
/// allows — no write descriptor on an image about to be exec'd — else copies),
/// `root/` (CAULDRON_HOME) and `home/` (HOME).
fn scratch(tag: &str) -> Scratch {
    let p = std::env::temp_dir().join(format!(
        "cauldron-ctl-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    for d in ["bin", "root", "home"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    let tui = tui_bin();
    for (from, name) in [(Path::new(CAULDRON), "cauldron"), (tui.as_path(), "tui")] {
        let to = p.join("bin").join(name);
        if std::fs::hard_link(from, &to).is_err() {
            std::fs::copy(from, &to).unwrap_or_else(|e| panic!("install {name}: {e}"));
        }
    }
    Scratch(p)
}

struct Out {
    rc: i32,
    stdout: String,
    stderr: String,
}

/// Run the installed `cauldron` with `args`, `stdin` piped in.
fn run(s: &Path, args: &[&str], stdin: &str, extra: &[(&str, &str)]) -> Out {
    let mut c = Command::new(s.join("bin").join("cauldron"));
    c.args(args)
        .env("CAULDRON_HOME", s.join("root"))
        .env("HOME", s.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("SOUP_SECRET")
        .env("CAULDRON_OPENER", "none")
        .env("CAULDRON_TYPE_ONE", "off")
        .env("TUI_THREADS_CHASSIS_PORT", "1")
        .env("AUI_V2_URL", "ws://127.0.0.1:1/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // REL-6a8: give the child exactly the listeners this test process bound
    // and granted (the scratch soup door), each pinned to this pid.
    c.env(env::egress::ALLOW_ENV, env::egress::grants_value(env::egress::granted()));
    for (k, v) in extra {
        c.env(k, v);
    }
    let mut child = c.spawn().expect("spawn the installed cauldron");
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    Out { rc: o.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&o.stdout).into_owned(), stderr: String::from_utf8_lossy(&o.stderr).into_owned() }
}

/// The installed face, directly.
fn tui(s: &Path, args: &[&str]) -> Out {
    let o = Command::new(s.join("bin").join("tui"))
        .args(args)
        .env("CAULDRON_HOME", s.join("root"))
        .env("HOME", s.join("home"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env("CAULDRON_OPENER", "none")
        .env("CAULDRON_TYPE_ONE", "off")
        .env("TUI_THREADS_CHASSIS_PORT", "1")
        .env("AUI_V2_URL", "ws://127.0.0.1:1/")
        .stdin(Stdio::null())
        .output()
        .expect("spawn the installed tui");
    Out { rc: o.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&o.stdout).into_owned(), stderr: String::from_utf8_lossy(&o.stderr).into_owned() }
}

fn create(s: &Path, name: &str) {
    let o = run(s, &["env", "create", name], "", &[]);
    assert_eq!(o.rc, 0, "env create {name}: {}{}", o.stdout, o.stderr);
}

fn data_root(s: &Path, env: &str) -> PathBuf {
    s.join("root").join("data").join(env)
}

fn log(s: &Path, env: &str) -> PathBuf {
    data_root(s, env).join("tui").join("soup").join("dumps.jsonl")
}

fn lines(s: &Path, env: &str) -> Vec<String> {
    std::fs::read_to_string(log(s, env)).unwrap_or_default().lines().map(str::to_string).collect()
}

fn masked(line: &str) -> Value {
    let mut v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("not a dump record ({e}): {line}"));
    let o = v.as_object_mut().expect("an object");
    assert!(o.remove("seq").is_some() && o.remove("at").is_some(), "no seq/at: {line}");
    v
}

const TEXT: &str = "the coordinator looked at the ledger";

/// **C1 + C4.**
#[test]
fn rv1s12a_c1_ctl_dump_is_the_faces_own_send_and_c4_the_frame_shows_it() {
    let s = scratch("c1");
    create(&s.0, "drift");

    // C4's opposition first: before any dump the Ledger has none.
    let before = run(&s.0, &["ctl", "frame", "--env", "drift", "--status-tab", "ledger"], "", &[]);
    assert_eq!(before.rc, 0, "{}", before.stderr);
    assert!(before.stdout.contains("no dump yet") && !before.stdout.contains(TEXT), "{}", before.stdout);

    // ctl's dump.
    let ctl = run(&s.0, &["ctl", "dump", "--env", "drift", "--note", "a second note", "--quote", "Q-1"], &format!("{TEXT}\n"), &[]);
    assert_eq!(ctl.rc, 0, "ctl dump: {}", ctl.stderr);
    let after = lines(&s.0, "drift");
    assert_eq!(after.len(), 1, "ctl dump appended exactly one line: {after:?}");
    assert_eq!(ctl.stdout.trim_end_matches('\n'), after[0], "ctl printed the line it appended, byte for byte");

    // The face's own send, the same bucket by hand.
    let face = tui(&s.0, &["--cauldron", "--env", "drift", "--dump", "--send", "--draft", TEXT, "--note", "a second note", "--quote", "Q-1"]);
    assert_eq!(face.rc, 0, "tui --send: {}", face.stderr);
    let after = lines(&s.0, "drift");
    assert_eq!(after.len(), 2);
    assert_ne!(after[0], after[1], "sentinel: two lines (seq/at differ)");
    assert_eq!(masked(&after[0]), masked(&after[1]), "ctl dump and the face's send wrote different dumps");

    // OPPOSED: one extra --quote on the ctl side.
    let more = run(&s.0, &["ctl", "dump", "--env", "drift", "--note", "a second note", "--quote", "Q-1", "--quote", "Q-2"], TEXT, &[]);
    assert_eq!(more.rc, 0, "{}", more.stderr);
    let after = lines(&s.0, "drift");
    assert_eq!(after.len(), 3);
    assert_ne!(masked(&after[1]), masked(&after[2]), "the comparison cannot see an extra quote");

    // C4: a dump with its own first line, then the Ledger frame.
    let c4 = run(&s.0, &["ctl", "dump", "--env", "drift"], "C4 sentinel: the newest dump\nits second line\n", &[]);
    assert_eq!(c4.rc, 0, "{}", c4.stderr);
    let frame = run(&s.0, &["ctl", "frame", "--env", "drift", "--status-tab", "ledger"], "", &[]);
    assert_eq!(frame.rc, 0, "{}", frame.stderr);
    let at = frame.stdout.find("C4 sentinel: the newest dump").unwrap_or_else(|| panic!("the frame does not show the new dump:\n{}", frame.stdout));
    // "At the top" needs a real element below it: the older dumps (TEXT)
    // must be IN the frame, or the ordering half passes on nothing
    // (RV1.S12a-I finding 2; RV1.S12c).
    let older = frame.stdout.find(TEXT).unwrap_or_else(|| panic!("the frame shows no older dump to be above, so \"at the top\" would compare against nothing:\n{}", frame.stdout));
    assert!(at < older, "the new dump is not at the top:\n{}", frame.stdout);
}

/// **C2: the demo is read-only**, and `ctl dump` never defaults to prod.
#[test]
fn rv1s12a_c2_ctl_dump_into_the_demo_is_refused_and_appends_nothing() {
    let s = scratch("c2");
    create(&s.0, "cauldron-build");
    create(&s.0, "fresh");
    let len = |e: &str| std::fs::metadata(log(&s.0, e)).map(|m| m.len()).unwrap_or(0);
    let before = len("cauldron-build");
    let o = run(&s.0, &["ctl", "dump", "--env", "cauldron-build"], TEXT, &[]);
    assert_ne!(o.rc, 0, "a dump into the demo was accepted: {}", o.stdout);
    assert!(o.stderr.contains("read-only"), "{}", o.stderr);
    assert_eq!(len("cauldron-build"), before, "the demo's log changed");
    // No --env and no named environment: refused, nothing written anywhere.
    let o = run(&s.0, &["ctl", "dump"], TEXT, &[]);
    assert_ne!(o.rc, 0, "ctl dump defaulted to an environment");
    assert!(o.stderr.contains("needs --env"), "{}", o.stderr);
    // OPPOSED: a fresh environment gains exactly one line.
    let o = run(&s.0, &["ctl", "dump", "--env", "fresh"], TEXT, &[]);
    assert_eq!(o.rc, 0, "{}", o.stderr);
    assert_eq!(lines(&s.0, "fresh").len(), 1);
    assert!(len("fresh") > 0);
}

// ── C3 / C5: a scratch soup door ───────────────────────────────────────────

const SECRET: &str = "ctl-test-reader-secret";

/// What the scratch door saw: each request's params.
type Seen = Arc<Mutex<Vec<Value>>>;

/// **A SCRATCH SOUP DOOR** on an ephemeral loopback port, speaking the soup's
/// frames as `services/soup/tests/door_e2e.rs` (soup/phase-d 4bc46bdd) drives
/// them: `welcome` → `auth` (secret + consumer) → `auth_ack` → `request` →
/// `response` correlated by id. It holds one node, `ws/task` `T-1`; a read of
/// any other id answers zero rows (as the soup's MATCH does); a read of type
/// `nope` is refused `malformed` (as the spine refuses an unknown type); a
/// wrong secret gets the soup's one refused-credential ack.
fn door() -> (u16, Seen) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    // REL-6a8: an isolated dial is admitted only to a listener its harness
    // bound and GRANTED; `run` hands the child every grant as
    // `addr:port@<this pid>` (`env::egress::ALLOW_ENV`). RV1.S12c.
    env::egress::grant_addr(l.local_addr().unwrap()).expect("grant the scratch soup door's listener");
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in l.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let Ok(mut ws) = tungstenite::accept(stream) else { return };
                let mut say = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, v: Value| ws.send(tungstenite::Message::Text(v.to_string())).is_ok();
                let hear = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>| -> Option<Value> {
                    loop {
                        match ws.read().ok()? {
                            tungstenite::Message::Text(t) => return serde_json::from_str(&t).ok(),
                            tungstenite::Message::Close(_) => return None,
                            _ => continue,
                        }
                    }
                };
                say(&mut ws, json!({"type": "welcome", "daemon": "soup", "auth_required": true, "boot_id": "scratch"}));
                let Some(auth) = hear(&mut ws) else { return };
                if auth["type"] != "auth" || auth["secret"] != SECRET || !auth["consumer"].is_object() {
                    say(&mut ws, json!({"type":"auth_ack","ok":false,"error":{"code":"unauthorized","message":"unauthorized: credentials not accepted"}}));
                    return;
                }
                say(&mut ws, json!({"type": "auth_ack", "ok": true}));
                while let Some(req) = hear(&mut ws) {
                    if req["type"] != "request" {
                        continue;
                    }
                    let p = req["params"].clone();
                    log.lock().unwrap().push(json!({"method": req["method"], "params": p}));
                    let st = p["statement"].as_str().unwrap_or("");
                    let outcome = if req["method"] != "cauldron/read" {
                        json!({"type": "error", "error": "no such method", "code": "unknown_method"})
                    } else if st.contains("ws/nope") {
                        json!({"type": "error", "error": "pool `ws` has no type `nope`", "code": "malformed"})
                    } else if st.starts_with("MATCH (n: ws/task { id: $id })") && p["bindings"]["id"] == "T-1" {
                        json!({"type": "success", "payload": {"rows": [{"n.id": "T-1", "REV(n)": 3, "n.title": "write the ctl verb"}], "retries": 0, "restarts": 0, "escalated": false}})
                    } else {
                        json!({"type": "success", "payload": {"rows": [], "retries": 0, "restarts": 0, "escalated": false}})
                    };
                    say(&mut ws, json!({"type": "response", "correlate": req["id"], "outcome": outcome}));
                }
            });
        }
    });
    (port, seen)
}

/// Register `addr` as `env`'s `soup` seam, the way a started soup's row reads.
fn seam(s: &Path, env: &str, addr: &str) {
    let dr = data_root(s, env);
    let norm = env::guard::normalize(&dr).unwrap().display().to_string();
    env::seams::update(env, &dr, &dr.join("seams.json"), |t| {
        t.seams.insert(
            "soup".into(),
            env::seams::Seam {
                service: "soup".into(),
                addr: addr.into(),
                pid: std::process::id(),
                boot_token: "scratch-boot".into(),
                version: "scratch".into(),
                commit: String::new(),
                dirty: false,
                binary: "scratch-door".into(),
                env: env.into(),
                data_root: norm.clone(),
                data_dir: norm.clone(),
                started_at: 0,
                probe: "none".into(),
                registered: false,
                pid_source: "self".into(),
                health: String::new(),
            },
        );
        Ok(())
    })
    .unwrap();
}

fn err_json(o: &Out) -> Value {
    let line = o.stderr.lines().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no JSON refusal on stderr: {}", o.stderr));
    serde_json::from_str(line).unwrap()
}

/// **C3: one node, read at the seam table's address.**
#[test]
fn rv1s12a_c3_ctl_node_reads_one_node_at_the_seam_tables_address() {
    let s = scratch("c3");
    create(&s.0, "nodes");
    let (port, seen) = door();
    seam(&s.0, "nodes", &format!("127.0.0.1:{port}"));
    let sec = [("SOUP_SECRET", SECRET)];

    let o = run(&s.0, &["ctl", "node", "T-1", "--env", "nodes", "--pool", "ws", "--type", "task", "--field", "title"], "", &sec);
    assert_eq!(o.rc, 0, "{}", o.stderr);
    let got: Value = serde_json::from_str(o.stdout.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {}", o.stdout));
    assert_eq!(got, json!({"id": "T-1", "pool": "ws", "type": "task", "node": {"n.id": "T-1", "REV(n)": 3, "n.title": "write the ctl verb"}}));
    let req = seen.lock().unwrap().first().cloned().expect("the door saw the read");
    assert_eq!(req["method"], "cauldron/read");
    assert_eq!(req["params"], json!({"statement": "MATCH (n: ws/task { id: $id }) RETURN n.id, REV(n), n.title", "bindings": {"id": "T-1"}, "format_version": 1}));
    assert!(!o.stdout.contains(SECRET) && !o.stderr.contains(SECRET), "the secret was printed");

    // An id the soup does not hold: non-zero, `not_found`.
    let o = run(&s.0, &["ctl", "node", "T-9", "--env", "nodes", "--pool", "ws", "--type", "task"], "", &sec);
    assert_eq!(o.rc, 1, "{}{}", o.stdout, o.stderr);
    assert_eq!(err_json(&o)["code"], "not_found");
    // The soup's own refusal: its code, verbatim.
    let o = run(&s.0, &["ctl", "node", "T-1", "--env", "nodes", "--pool", "ws", "--type", "nope"], "", &sec);
    assert_eq!(o.rc, 1, "{}{}", o.stdout, o.stderr);
    assert_eq!(err_json(&o), json!({"code": "malformed", "message": "pool `ws` has no type `nope`"}));
    // A wrong secret: the door's refused-credential ack, verbatim.
    let o = run(&s.0, &["ctl", "node", "T-1", "--env", "nodes", "--pool", "ws", "--type", "task"], "", &[("SOUP_SECRET", "not-it")]);
    assert_eq!(o.rc, 1);
    assert_eq!(err_json(&o)["code"], "unauthorized");
    // No seam row: refused before any dial.
    create(&s.0, "empty");
    let o = run(&s.0, &["ctl", "node", "T-1", "--env", "empty", "--pool", "ws", "--type", "task"], "", &sec);
    assert_ne!(o.rc, 0);
    assert!(o.stderr.contains("no `soup` seam"), "{}", o.stderr);
}

/// **C5: the egress rule decides the dial.** A seam row off loopback
/// (TEST-NET-1, never routed) is refused by `env::egress` before anything is
/// dialled; opposed by C3, whose loopback row recorded in the scratch root is
/// admitted.
#[test]
fn rv1s12a_c5_ctl_node_dials_only_what_isolation_admits() {
    let s = scratch("c5");
    create(&s.0, "far");
    seam(&s.0, "far", "192.0.2.1:4000");
    let o = run(&s.0, &["ctl", "node", "T-1", "--env", "far", "--pool", "ws", "--type", "task"], "", &[("SOUP_SECRET", SECRET)]);
    assert_ne!(o.rc, 0, "an off-loopback soup was dialled: {}", o.stdout);
    assert!(o.stderr.contains("refused") && o.stderr.contains("not loopback"), "{}", o.stderr);
}
