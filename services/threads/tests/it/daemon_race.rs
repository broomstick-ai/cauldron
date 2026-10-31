//! THE DUPLICATE-DAEMON RACE — what the loser of the port race is allowed to do.
//!
//! ## The race
//!
//! When the daemon self-updates it `execv`s itself, so port
//! [`threads::config::DEFAULT_PORT`] is unheld for roughly 25 milliseconds.
//! AUI's health probe lands in that gap, concludes the daemon is dead, and
//! spawns a second one. On 2026-08-20 at 21:36:41 that second daemon was pid
//! 79138 (`~/.aui/launchd.log`: "threads daemon spawned (detached) pid=79138"),
//! and pid 79138 is the loser recorded in `lib/threads/restarts.jsonl`. It has
//! fired on 12 of the last 13 self-updates.
//!
//! The race is not the bug. Two daemons racing for one port is fine — that is
//! what the bind is FOR. The bug was the ORDER: `run_daemon` built the manager
//! and ran the update protocol's boot half BEFORE it took the port, so by the
//! time the loser discovered it had lost it had already opened the live
//! `threads.db` read-write, run `apply_schema()`, possibly rewritten
//! `accounts.toml`, cleared the durable queued-restart record, overwritten
//! `threads.last-good` and appended to `restarts.jsonl`. `server::bind`'s own
//! doc comment claimed a losing daemon "dies having touched nothing"; that
//! sentence was false from the day it was written, and this file is what makes
//! it true.
//!
//! ## The two contracts here
//!
//! 1. [`a_daemon_that_loses_the_port_race_writes_nothing`] — the bind is the
//!    FIRST thing `run_daemon` does. Its evidence is the absence of a data
//!    directory: not the store, not the registry, not the ledger, not
//!    last-good. Anything that moves a write back above the bind fails it.
//! 2. [`two_writers_never_interleave_a_ledger_line`] — `log_event` issues ONE
//!    `write_all`. `writeln!` on a raw `File` formats straight into the fd, and
//!    `serde_json`'s `Display` emits token by token, so a single logged line was
//!    a dozen small `O_APPEND` writes that two processes interleaved
//!    character-by-character. It corrupted the ledger on 2026-08-04 and again on
//!    2026-08-20 — and the second corruption destroyed the two `boot` lines that
//!    would have made this whole episode obvious on sight.

use std::path::PathBuf;
use std::sync::Arc;

use threads::update::UpdateGate;
use threads::Config;

// ── 1. the bind is first ──────────────────────────────────────────────────

/// **THE PRIMARY FIX.** A daemon that loses the port race must die at the bind
/// having written nothing at all.
///
/// The incumbent is stood in for by a listener this test holds for the whole
/// call, which is exactly the incumbent's shape: something else already owns
/// the addr, so `TcpListener::bind` returns `EADDRINUSE`.
///
/// The assertion is deliberately the DIRECTORY, not a list of files. A list
/// goes stale the moment the daemon learns to write something new at boot; "the
/// data dir was never even created" cannot. `build_manager` is what calls
/// `create_dir_all`, so its very existence is the proof that something ran
/// ahead of the bind.
#[tokio::test]
async fn a_daemon_that_loses_the_port_race_writes_nothing() {
    // THE CONSUMER LEDGER IS WIRED BY `run_daemon`, so a test that starts one
    // must be pointed away from the operator's real `~/.cauldron` — otherwise
    // every suite run appends to the file he uses to see who is subscribed, and
    // parallel test binaries interleave into it. See `threads::consumers`.
    std::env::set_var(
        "CAULDRON_HOME",
        std::env::temp_dir().join("threads-tests-cauldron"),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("threads-home");

    // The incumbent daemon: it holds the port, and it keeps holding it.
    let incumbent = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the incumbent must be able to take a port");
    let port = incumbent.local_addr().expect("local_addr").port();

    let config = Config {
        host: "127.0.0.1".to_string(),
        chassis_port: port,
        data_dir: Some(data_dir.to_string_lossy().to_string()),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };

    // THE CHASSIS PORT COMES FROM THIS CONFIG, NOT THE ENVIRONMENT. `run_daemon`
    // reads `THREADS_CHASSIS_PORT` since R2.S4; an inherited value would make
    // this daemon bind someone else's port — an inspector's safety guard of
    // `=1` made this test red for the wrong reason and blind to the bind order.
    std::env::remove_var("THREADS_CHASSIS_PORT");
    let err = threads::run_daemon(config)
        .await
        .expect_err("the loser must not start serving");
    let msg = err.to_string();
    assert!(
        msg.contains(&format!("binding 127.0.0.1:{port}")),
        "the loser must die AT THE BIND, not somewhere later; got: {msg}"
    );

    // The whole contract, in one line.
    assert!(
        !data_dir.exists(),
        "the loser touched its data dir before losing — it contains {:?}. \
         Every write in run_daemon must sit BELOW server::bind.",
        std::fs::read_dir(&data_dir)
            .map(|d| d
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );

    drop(incumbent);
}

// ── 2. one line, one write ────────────────────────────────────────────────

/// A file that is a plausible `exe` for a gate without being 50 MB of test
/// harness. `log_event` only ever prints its path.
fn stand_in_binary(dir: &std::path::Path) -> PathBuf {
    let p = dir.join("threads");
    std::fs::write(&p, b"not really a Mach-O").expect("write stand-in");
    p
}

/// **THE LEDGER CONTRACT.** Concurrent appenders must never interleave.
///
/// Proven at N = 2 writers minimum — an arbitrator is proven under contention
/// or it is not proven, and a single writer cannot exercise the thing this
/// exists to manage. Threads rather than processes: the mechanism is identical
/// (independent fds, `O_APPEND`, one `write` per formatting fragment), and the
/// kernel does not care which side of a process boundary the fd came from.
#[test]
fn two_writers_never_interleave_a_ledger_line() {
    const WRITERS: usize = 4;
    const LINES_EACH: usize = 250;

    // THE CONSUMER LEDGER IS WIRED BY `run_daemon`, so a test that starts one
    // must be pointed away from the operator's real `~/.cauldron` — otherwise
    // every suite run appends to the file he uses to see who is subscribed, and
    // parallel test binaries interleave into it. See `threads::consumers`.
    std::env::set_var(
        "CAULDRON_HOME",
        std::env::temp_dir().join("threads-tests-cauldron"),
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir");
    let gate: Arc<UpdateGate> =
        UpdateGate::for_binary(data_dir.clone(), stand_in_binary(tmp.path()));

    let mut handles = Vec::new();
    for w in 0..WRITERS {
        let gate = Arc::clone(&gate);
        handles.push(std::thread::spawn(move || {
            for i in 0..LINES_EACH {
                // A payload with some bulk to it: the corruption is a race
                // between the small writes ONE line is chopped into, so a line
                // that fits in a single write would hide the bug.
                gate.log_event(
                    "boot",
                    serde_json::json!({
                        "writer": w,
                        "i": i,
                        "identity": format!("cdhash:{:0>40x}", w * 1000 + i),
                        "note": "a new daemon binary is on disk, and this is the padding \
                                 that makes one ledger line long enough to be chopped up",
                        "pending": false,
                    }),
                );
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread");
    }

    let text = std::fs::read_to_string(gate.log_path()).expect("read the ledger");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();

    let mut bad = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if serde_json::from_str::<serde_json::Value>(line).is_err() {
            bad.push((n + 1, line.chars().take(120).collect::<String>()));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} ledger lines are not JSON — two writers interleaved inside one \
         line. log_event must build the line first and issue exactly one write_all. \
         First offenders: {:?}",
        bad.len(),
        lines.len(),
        &bad[..bad.len().min(3)]
    );
    assert_eq!(
        lines.len(),
        WRITERS * LINES_EACH,
        "every logged event must be exactly one line — no line lost, none split in two"
    );
}
