//! **DG-2 — SERVICE AND DAEMON LOGS ROTATE BY RENAME AT A SIZE BOUND.**
//!
//! The helper (`cli/src/logrotate.rs`) is included by `#[path]`, because the
//! cli has no library target; its four guards are the brief's four. The last
//! test drives the real `cauldron build / pin / start` of a hosted fake, so a
//! call site that stopped using the helper goes red here too.
//!
//! Every test runs in its own temp dir (a throwaway `CAULDRON_HOME` for the
//! end-to-end one). Nothing touches `~/.cauldron`, a live daemon or launchd.
//! Oversized logs are SPARSE (`set_len`), so a 64 MiB bound costs no disk.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../../src/logrotate.rs"]
#[allow(dead_code)]
mod logrotate;
use logrotate::{generation_path, open_log, rotate_if_over, LOG_GENERATIONS, LOG_MAX_BYTES};

#[path = "support/launch_wait.rs"]
mod launch_wait;
use launch_wait::START_TIMEOUT_MS;

struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        if std::env::var_os("KEEP_SANDBOX").is_none() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn tmp(tag: &str) -> Tmp {
    let root = std::env::temp_dir().join(format!(
        "cauldron-dg2-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    assert!(!root.starts_with(home.join(".cauldron")), "sandbox must not be ~/.cauldron");
    std::fs::create_dir_all(&root).unwrap();
    Tmp(root)
}

/// A file that starts with `marker` and is `len` bytes long (sparse past it).
fn sized(path: &Path, marker: &str, len: u64) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(marker.as_bytes()).unwrap();
    f.set_len(len).unwrap();
}

fn head(path: &Path, n: usize) -> String {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    String::from_utf8_lossy(&b[..n.min(b.len())]).into_owned()
}

fn len(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).len()
}

/// GUARD 1: a log one byte past the bound is renamed to `.1` and the path
/// opens as a fresh, empty file. Forced red by making the rotation a no-op.
#[test]
fn dg2_a_a_log_one_byte_past_the_bound_rotates_to_dot_1() {
    let t = tmp("over");
    let log = t.0.join("svc.log");
    sized(&log, "OLD-LIVE", LOG_MAX_BYTES + 1);
    let f = open_log(&log).unwrap();
    drop(f);
    let one = generation_path(&log, 1);
    assert!(one.exists(), "{} was not created", one.display());
    assert_eq!(head(&one, 8), "OLD-LIVE");
    assert_eq!(len(&one), LOG_MAX_BYTES + 1, "the rotated generation is not the old live file");
    assert_eq!(len(&log), 0, "the live path was not reopened fresh");
}

/// GUARD 2 (opposition to 1): a log AT the bound, and one well under it, are
/// left exactly as they are, and no `.1` appears. Red under a rotation that
/// fires unconditionally.
#[test]
fn dg2_b_a_log_at_or_under_the_bound_is_not_rotated() {
    let t = tmp("under");
    for (name, size) in [("at.log", LOG_MAX_BYTES), ("small.log", 100)] {
        let log = t.0.join(name);
        sized(&log, "KEEP", size);
        let mut f = open_log(&log).unwrap();
        f.write_all(b"x").unwrap();
        drop(f);
        assert!(!generation_path(&log, 1).exists(), "{name} rotated at {size} bytes");
        assert_eq!(head(&log, 4), "KEEP");
        assert_eq!(len(&log), size + 1, "{name} was not appended to in place");
    }
    // And a log that does not exist yet is simply created.
    let fresh = t.0.join("fresh.log");
    drop(open_log(&fresh).unwrap());
    assert!(fresh.exists() && !generation_path(&fresh, 1).exists());
}

/// GUARD 3: with `.1`, `.2`, `.3` and a stale `.4` present, a rotation keeps the
/// NEWEST [`LOG_GENERATIONS`] — the old live file becomes `.1`, the old `.1`
/// becomes `.2`, the old `.2` becomes `.3` — and nothing past `.3` remains. Forced red by removing the
/// cap (every generation shifted up and kept).
#[test]
fn dg2_c_generations_past_the_cap_of_3_are_removed_and_the_newest_kept() {
    assert_eq!(LOG_GENERATIONS, 3, "this test's fixture is written for the DG-2-1 ruling of 3");
    let t = tmp("gens");
    let log = t.0.join("svc.log");
    sized(&log, "GEN-LIVE", LOG_MAX_BYTES + 1);
    sized(&generation_path(&log, 1), "GEN-ONE-", 10);
    sized(&generation_path(&log, 2), "GEN-TWO-", 10);
    sized(&generation_path(&log, 3), "GEN-THRE", 10);
    sized(&generation_path(&log, 4), "GEN-FOUR", 10);
    assert!(rotate_if_over(&log, LOG_MAX_BYTES, LOG_GENERATIONS).unwrap());
    assert!(!log.exists(), "the live file was not moved");
    assert_eq!(head(&generation_path(&log, 1), 8), "GEN-LIVE");
    assert_eq!(head(&generation_path(&log, 2), 8), "GEN-ONE-");
    assert_eq!(head(&generation_path(&log, 3), 8), "GEN-TWO-");
    for n in 4..=6 {
        assert!(!generation_path(&log, n).exists(), "generation .{n} survived past the cap");
    }
    // A second rotation keeps bounding it: the oldest (GEN-TWO-) drops.
    sized(&log, "GEN-NEXT", LOG_MAX_BYTES + 1);
    assert!(rotate_if_over(&log, LOG_MAX_BYTES, LOG_GENERATIONS).unwrap());
    assert_eq!(head(&generation_path(&log, 1), 8), "GEN-NEXT");
    assert_eq!(head(&generation_path(&log, 2), 8), "GEN-LIVE");
    assert_eq!(head(&generation_path(&log, 3), 8), "GEN-ONE-");
    assert!(!generation_path(&log, 4).exists(), "generation .4 exists after a second rotation");
}

/// GUARD 4: a writer that holds the log open across a rotation keeps writing,
/// in order and intact, to the file it holds (now `.1`); the NEXT open writes
/// a fresh live file holding only its own lines. Forced red by rotating with
/// copy-then-truncate-in-place: the held writer's later line lands in the
/// truncated live file and is missing from `.1`.
#[test]
fn dg2_d_an_open_writer_keeps_writing_across_a_rotation_by_rename() {
    let t = tmp("held");
    let log = t.0.join("svc.log");
    sized(&log, "HELD-START\n", LOG_MAX_BYTES - 10);
    let mut held = open_log(&log).unwrap();
    held.write_all(b"\nbefore-rotation\n").unwrap(); // now past the bound
    assert!(len(&log) > LOG_MAX_BYTES, "sentinel: the held file must be over the bound");
    // The writer's NEXT open (a restart) rotates the path out from under it.
    let mut next = open_log(&log).unwrap();
    held.write_all(b"after-rotation\n").unwrap();
    next.write_all(b"next-open\n").unwrap();
    drop(held);
    drop(next);
    let one = generation_path(&log, 1);
    let old = std::fs::read(&one).unwrap();
    let tail = String::from_utf8_lossy(&old[old.len().saturating_sub(64)..]).into_owned();
    assert!(
        tail.ends_with("\nbefore-rotation\nafter-rotation\n"),
        "the held writer did not keep writing to its own file: tail of .1 = {tail:?}"
    );
    assert_eq!(head(&one, 11), "HELD-START\n", "the rotated file lost its head");
    let live = std::fs::read_to_string(&log).unwrap();
    assert_eq!(live, "next-open\n", "the live file holds something other than the next open's lines");
}

// ── end to end: the real `cauldron start` of a hosted fake ────────────────

const FAKE: &str = r#"#!/usr/bin/env python3
import os, socket, sys, time
port = int(os.environ["FAKE_PORT"])
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen(8)
with open(os.environ["FAKE_PIDS"], "a") as f:
    f.write(f"{os.getpid()}\n")
print("FAKE-STARTED", flush=True)
while True:
    try:
        c, _ = s.accept(); c.close()
    except OSError:
        time.sleep(0.05)
"#;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn listening(port: u16) -> bool {
    let sa: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    TcpStream::connect_timeout(&sa, Duration::from_millis(200)).is_ok()
}

struct Sandbox {
    t: Tmp,
    env: String,
    pids: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = self.cauldron(&["stop", "svc"]);
        for pid in std::fs::read_to_string(&self.pids).unwrap_or_default().lines() {
            let _ = Command::new("/bin/kill").args(["-9", pid]).stderr(Stdio::null()).status();
        }
    }
}

impl Sandbox {
    fn cauldron(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_cauldron"))
            .args(args)
            .env("CAULDRON_HOME", &self.t.0)
            .env("CAULDRON_ENV", &self.env)
            .output()
            .expect("spawn cauldron");
        (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

/// GUARD 1 at a real call site: `cauldron start` of a hosted service whose
/// log is one byte past the bound rotates it to `.1`, and the started
/// service's own output lands in the fresh live log. Forced red with the
/// rotation disabled (the same forcing as guard 1).
#[test]
fn dg2_e_cauldron_start_rotates_a_hosted_services_oversized_log() {
    let t = tmp("e2e");
    let root = t.0.clone();
    let env = "dg2e".to_string();
    let sb = Sandbox { t, env: env.clone(), pids: root.join("pids") };
    std::fs::create_dir_all(root.join("services")).unwrap();
    std::fs::create_dir_all(root.join("data").join(&env).join("logs")).unwrap();
    std::fs::write(root.join("data").join(&env).join("env.toml"), "[supervisor]\nauto_restart = false\n").unwrap();
    let fake = root.join("fake-daemon");
    std::fs::write(&fake, launch_wait::slow_launch_py(FAKE, &root.join("f2g-marks"))).unwrap();
    Command::new("/bin/chmod").args(["+x"]).arg(&fake).status().unwrap();
    let port = free_port();
    std::fs::write(
        root.join("services").join("svc.toml"),
        format!(
            "name = \"svc\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
             stop_signal = \"TERM\"\nstart_timeout_ms = {START_TIMEOUT_MS}\n\
             [launch]\naddr = \"127.0.0.1:{port}\"\n\
             [env]\nFAKE_PORT = \"{port}\"\nFAKE_PIDS = \"{}\"\n\
             [build]\nartifact = \"{}\"\nbin = \"fake-daemon\"\n",
            sb.pids.display(),
            fake.display()
        ),
    )
    .unwrap();
    let (rc, out) = sb.cauldron(&["build", "svc"]);
    assert_eq!(rc, 0, "build: {out}");
    let (rc, out) = sb.cauldron(&["versions", "svc", "--json"]);
    assert_eq!(rc, 0, "versions: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    let label = v[0]["label"].as_str().expect("a label").to_string();
    let (rc, out) = sb.cauldron(&["pin", "svc", &label]);
    assert_eq!(rc, 0, "pin: {out}");

    let log = root.join("data").join(&env).join("logs").join("svc.log");
    sized(&log, "E2E-OLD-LOG", LOG_MAX_BYTES + 1);
    let (rc, out) = sb.cauldron(&["start", "svc"]);
    assert_eq!(rc, 0, "start: {out}");
    let budget = launch_wait::START_TIMEOUT + Duration::from_secs(30);
    let t0 = Instant::now();
    while !(listening(port) && std::fs::read_to_string(&log).map(|s| s.contains("FAKE-STARTED")).unwrap_or(false)) {
        assert!(t0.elapsed() < budget, "svc never came up writing to {} within {budget:?}: {out}", log.display());
        std::thread::sleep(Duration::from_millis(100));
    }
    let one = generation_path(&log, 1);
    assert!(one.exists(), "start did not rotate {}: {out}", log.display());
    assert_eq!(head(&one, 11), "E2E-OLD-LOG");
    assert_eq!(len(&one), LOG_MAX_BYTES + 1);
    assert!(len(&log) < 4096, "the live log was not fresh: {} bytes", len(&log));
}

// ═══════════════════════════════════════ DG-2b: THE HOLDER'S LAUNCHD LOG ════
//
// The real `cauldron daemon --once` (prod, in a throwaway HOME and
// CAULDRON_HOME) started the way launchd starts it: fd 1 and fd 2 are ONE
// append descriptor on `<data root>/logs/cauldron.launchd.log`, which this
// test holds open the whole time, standing in for launchd's open. Nothing
// here touches launchd, `~/.cauldron` or a live holder.

/// A throwaway home with prod created; returns it and its launchd log path.
fn holder_home(tag: &str) -> (Tmp, PathBuf) {
    let t = tmp(tag);
    let (rc, out) = holder_cmd(&t.0, &["env", "create", "prod"]).output().map(|o| {
        (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }).unwrap();
    assert_eq!(rc, 0, "env create prod: {out}");
    let logs = t.0.join(".cauldron").join("data").join("prod").join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    (t, logs.join("cauldron.launchd.log"))
}

fn holder_cmd(home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_cauldron"));
    c.args(args)
        .env("HOME", home)
        .env("CAULDRON_HOME", home.join(".cauldron"))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .env_remove("CAULDRON_RELAUNCHED_FROM")
        .env_remove("CAULDRON_LAUNCHD_LOG_ROTATED");
    c
}

/// `cauldron daemon --once` with fd 1 and fd 2 both on `stdio`, as launchd
/// spawns the holder. Returns the exit code.
fn start_holder(home: &Path, stdio: &std::fs::File) -> i32 {
    holder_cmd(home, &["daemon", "--once"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdio.try_clone().unwrap()))
        .stderr(Stdio::from(stdio.try_clone().unwrap()))
        .status()
        .expect("spawn cauldron daemon")
        .code()
        .unwrap_or(-1)
}

/// launchd's open of `StandardOutPath`: create + append.
fn launchd_open(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap()
}

fn inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).ino()
}

fn text(path: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).into_owned()
}

/// The last 2 KiB of a (possibly 64 MiB, sparse) file, for a failure message.
fn tail_of(path: &Path) -> String {
    let b = std::fs::read(path).unwrap_or_default();
    String::from_utf8_lossy(&b[b.len().saturating_sub(2048)..]).into_owned()
}

const ONE_PASS: &str = "--once, so one pass and out";

/// GUARD (startup path) + RENAME-NOT-TRUNCATE: a holder started onto a
/// launchd log one byte past the bound renames it to `.1` — the SAME inode,
/// every byte kept — and re-execs onto a fresh file at the path, so its whole
/// run lands in the new file and not one byte in `.1`. A descriptor still
/// open on the old file (launchd's, here the test's) keeps writing into `.1`.
/// Forced red by removing the `rotate_launchd_log_at_start` call from
/// `verb_daemon`, and by a copy-then-truncate rotation (the inode check).
#[test]
fn dg2b_a_holder_start_rotates_an_oversized_launchd_log_by_rename_onto_a_fresh_file() {
    let (_t, log) = holder_home("dg2b-start-over");
    sized(&log, "OLD-LAUNCHD-LOG", LOG_MAX_BYTES + 1);
    let was = inode(&log);
    let held = launchd_open(&log);
    let rc = start_holder(&_t.0, &held);
    let one = generation_path(&log, 1);
    assert!(one.exists(), "holder start did not rotate {} (rc {rc}); live log ends:\n{}", log.display(), tail_of(&log));
    assert_eq!(rc, 0, "holder --once failed: {}", tail_of(&log));
    assert_eq!(inode(&one), was, "`.1` is not the old file's inode: the rotation copied rather than renamed");
    assert_eq!(head(&one, 15), "OLD-LAUNCHD-LOG");
    assert_eq!(len(&one), LOG_MAX_BYTES + 1, "the holder wrote into the rotated file after rotating it");
    assert_ne!(inode(&log), was, "the live path is still the old file");
    let live = text(&log);
    assert!(live.contains("at holder start (past 64 MiB)"), "the re-exec'd holder did not say it rotated: {live}");
    assert!(live.contains(ONE_PASS), "the holder's run did not land in the fresh file: {live}");
    // launchd's descriptor on the old file still writes, into `.1`.
    (&held).write_all(b"after-rotation\n").unwrap();
    assert!(text(&one).ends_with("after-rotation\n"), "a held descriptor's write did not land in .1");
    assert!(!text(&log).contains("after-rotation"), "a held descriptor's write landed in the fresh file");
}

/// OPPOSITION: under the bound, holder start leaves the launchd log alone —
/// no `.1`, same inode, the holder appending to it as before. Forced red by
/// a rotation that always fires.
#[test]
fn dg2b_b_holder_start_leaves_an_undersized_launchd_log_alone() {
    let (_t, log) = holder_home("dg2b-start-under");
    std::fs::write(&log, "SMALL-LAUNCHD-LOG\n").unwrap();
    let was = inode(&log);
    let held = launchd_open(&log);
    let rc = start_holder(&_t.0, &held);
    assert_eq!(rc, 0, "holder --once failed: {}", tail_of(&log));
    assert!(!generation_path(&log, 1).exists(), "an under-bound launchd log was rotated");
    assert_eq!(inode(&log), was);
    let live = text(&log);
    assert!(live.starts_with("SMALL-LAUNCHD-LOG\n"), "{live}");
    assert!(live.contains(ONE_PASS), "the holder did not write its run to the launchd log: {live}");
    assert!(!live.contains("at holder start"), "{live}");
}

/// OPPOSITION: a holder whose stdio is NOT the launchd log (run by hand into
/// a terminal or another file) is not that file's writer and rotates
/// nothing, however large it is. Forced red by removing the fd-identity gate.
#[test]
fn dg2b_c_a_holder_not_writing_the_launchd_log_does_not_rotate_it() {
    let (_t, log) = holder_home("dg2b-start-other");
    sized(&log, "OLD-LAUNCHD-LOG", LOG_MAX_BYTES + 1);
    let was = inode(&log);
    let other = _t.0.join("by-hand.log");
    let rc = start_holder(&_t.0, &launchd_open(&other));
    assert!(!generation_path(&log, 1).exists(), "a holder not writing the launchd log rotated it");
    assert_eq!(inode(&log), was);
    assert_eq!(rc, 0, "holder --once failed: {}", text(&other));
    assert!(text(&other).contains(ONE_PASS), "{}", text(&other));
    assert_eq!(len(&log), LOG_MAX_BYTES + 1);
}

/// KEEPS EXACTLY 3, on the startup path: `.1 .2 .3` already there, the live
/// log over the bound → old live is `.1`, old `.1` is `.2`, old `.2` is
/// `.3`, old `.3` is gone and there is no `.4`. Forced red by keep-all (no
/// cap) and by keep-1.
#[test]
fn dg2b_d_holder_start_keeps_exactly_3_launchd_log_generations() {
    let (_t, log) = holder_home("dg2b-start-gens");
    for n in 1..=3 {
        std::fs::write(generation_path(&log, n), format!("GEN{n}")).unwrap();
    }
    sized(&log, "OLD-LAUNCHD-LOG", LOG_MAX_BYTES + 1);
    let held = launchd_open(&log);
    let rc = start_holder(&_t.0, &held);
    assert_eq!(rc, 0, "holder --once failed: {}", tail_of(&log));
    assert_eq!(head(&generation_path(&log, 1), 15), "OLD-LAUNCHD-LOG");
    assert_eq!(text(&generation_path(&log, 2)), "GEN1", "old .1 is not .2");
    assert_eq!(text(&generation_path(&log, 3)), "GEN2", "old .2 is not .3");
    assert!(!generation_path(&log, 4).exists(), "a 4th generation survived (old .3 was kept)");
    assert_eq!(LOG_GENERATIONS, 3);
    assert!(text(&log).contains(ONE_PASS));
}
