//! **AN ACCOUNT DISABLE SURVIVES A THREADS RESTART** (R4.S9, HOLDS R24).
//!
//! What happened (RECOMMENDATIONS-LEDGER, H179, 2026-10-15): the operator's
//! `alice-example-com` was disabled with `accounts/alice-example-com/disable` at
//! 18:17Z; threads was rebuilt, pinned and restarted at 18:45Z; afterwards a
//! second disable answered `changed: true` — the daemon that came up did not
//! know the account was out of the pool.
//!
//! The claim this file holds is the operator's, end to end, against the REAL
//! binary: disable through the door, stop the process the way a supervisor
//! does (SIGTERM, then wait for exit), start a NEW process on the same data
//! dir, and the account is still out of the pool. Two witnesses after the
//! restart, both read by the second process or off its disk:
//!
//! 1. the second process's own answer to a re-disable is `changed: false` —
//!    the exact observable that was wrong in prod;
//! 2. the row in `account_disabled` still carries the FIRST `disabled_at`, so
//!    the re-disable did not quietly re-create it.
//!
//! Throwaway everything: a temp data dir, a port taken from the kernel, a stub
//! `claude`, a temp `CAULDRON_HOME`. Nothing here can reach `:3653` or the
//! operator's `~/.cauldron`.
//!
//! **Forced red** (pasted in an internal report): the second boot
//! pointed at a fresh store (the "restart lost it" condition) → witness 1 reads
//! `changed: true`; and, opposing it, the enable leg with its enable removed
//! reads `changed: false` — so neither a daemon that forgets nor one that
//! answers a constant passes.
//!
//! **What this did NOT find** (R4.S9): on this tree, against a clean store, the
//! disable survives a clean restart AND the supervisor's launch→stop→launch.
//! The code has one writer of `account_disabled` (`Store::set_account_disabled`)
//! and one deleter (its `false` arm, reached only from `accounts/{a}/enable`),
//! no cascade and no boot-time clear. So prod's loss was not a restart dropping
//! a committed row in this code; see the report for what is left to read.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const ACCOUNT: &str = "alice-example-com";

fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude"))
}

/// A port the kernel just handed out and we let go of. The daemon binds it a
/// moment later; a collision makes the boot fail loudly, never a wrong pass.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_config(dir: &Path, data_dir: &Path, port: u16) -> PathBuf {
    let slot = dir.join("slots").join(ACCOUNT);
    std::fs::create_dir_all(&slot).unwrap();
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "host = \"127.0.0.1\"\n\
             chassis_port = {port}\n\
             data_dir = \"{}\"\n\
             claude_bin = \"{}\"\n\
             calibrate = false\n\
             edge_poll = false\n\
             probe_interval_secs = 0\n\
             probe_interval_busy_secs = 0\n\
             [accounts]\n\
             \"{ACCOUNT}\" = \"{}\"\n",
            data_dir.display(),
            fake_claude().display(),
            slot.display(),
        ),
    )
    .unwrap();
    cfg
}

struct Daemon {
    child: Child,
    port: u16,
}

impl Daemon {
    async fn boot(scratch: &Path, data_dir: &Path) -> Daemon {
        let port = free_port();
        let cfg = write_config(scratch, data_dir, port);
        let child = Command::new(env!("CARGO_BIN_EXE_threads"))
            .arg("--config")
            .arg(&cfg)
            // Never the operator's: his port, his cauldron, his claude home.
            .env_remove("THREADS_CHASSIS_PORT")
            .env("CAULDRON_HOME", scratch.join("cauldron-home"))
            .env("THREADS_CLAUDE_BIN", fake_claude())
            .env("THREADS_CLAUDE_HOME", scratch.join("claude-home"))
            .env("HOME", scratch.join("home"))
            .env("FAKE_STATE_DIR", scratch)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(scratch.join(format!("daemon-{port}.log"))).unwrap())
            .spawn()
            .expect("spawn the threads binary");
        let d = Daemon { child, port };
        // Up means the door answers, not that the process exists.
        // R9.C7g r37: and up means RECOVERED — the door opens while boot
        // recovery still runs behind it (C7g), so the daemon's own
        // `/health` `recovery.state == "done"` is what ends the wait.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if connect_async(format!("ws://127.0.0.1:{port}/ws")).await.is_ok() && recovered(port) {
                return d;
            }
            assert!(
                Instant::now() < deadline,
                "the throwaway daemon never opened its door on :{port}; log: {}",
                std::fs::read_to_string(scratch.join(format!("daemon-{port}.log")))
                    .unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// One request on a fresh connection; the whole outcome object.
    async fn request(&self, method: &str, params: Value) -> Value {
        let (ws, _) = connect_async(format!("ws://127.0.0.1:{}/ws", self.port))
            .await
            .unwrap();
        let (mut wr, mut rd) = ws.split();
        wr.send(Message::Text(
            json!({"type": "request", "id": "r24", "method": method, "params": params})
                .to_string(),
        ))
        .await
        .unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(20), rd.next())
                .await
                .expect("timed out waiting for the response")
                .expect("socket closed")
                .expect("socket error");
            if let Message::Text(t) = msg {
                let f: Value = serde_json::from_str(&t).unwrap();
                if f["type"] == "response" && f["correlate"] == json!("r24") {
                    return f["outcome"].clone();
                }
            }
        }
    }

    async fn changed(&self, method: &str) -> Value {
        let out = self.request(method, json!({})).await;
        assert_eq!(out["type"], "success", "{method}: {out}");
        out["payload"]["changed"].clone()
    }

    /// What a supervisor does: SIGTERM, then wait for the process to be gone.
    fn stop(mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("the throwaway daemon ignored SIGTERM for 30 s");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn disabled_at(data_dir: &Path) -> Option<String> {
    threads::Store::open(data_dir.join("threads.db"))
        .unwrap()
        .account_disabled_at(ACCOUNT)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disable_survives_a_restart_of_the_real_daemon() {
    let scratch = tempfile::tempdir().unwrap();
    let data_dir = scratch.path().join("threads-home");
    let disable = format!("accounts/{ACCOUNT}/disable");

    let first = Daemon::boot(scratch.path(), &data_dir).await;
    assert_eq!(first.changed(&disable).await, json!(true), "first disable");
    first.stop();
    let stamped = disabled_at(&data_dir).expect("the row is on disk after a clean stop");

    let second = Daemon::boot(scratch.path(), &data_dir).await;
    assert_eq!(
        second.changed(&disable).await,
        json!(false),
        "the restarted daemon must already know {ACCOUNT} is out of the pool"
    );
    second.stop();
    assert_eq!(
        disabled_at(&data_dir).as_deref(),
        Some(stamped.as_str()),
        "the re-disable must find the original row, not re-create it"
    );
}

/// **THE PROD SEQUENCE, NOT A TIDY ONE.** `cauldron supervisions --service
/// threads` for both of 2026-10-15's restarts reads: stop (repin) → launch →
/// stop (requested) ~10 ms later → launch. A daemon is SIGTERMed while it is
/// still booting, then a third process boots on the same store. The disable
/// set before all of it must still be there.
#[tokio::test(flavor = "multi_thread")]
async fn a_disable_survives_the_supervisors_launch_stop_launch() {
    let scratch = tempfile::tempdir().unwrap();
    let data_dir = scratch.path().join("threads-home");
    let disable = format!("accounts/{ACCOUNT}/disable");

    let first = Daemon::boot(scratch.path(), &data_dir).await;
    assert_eq!(first.changed(&disable).await, json!(true), "first disable");
    first.stop();

    // Launched and stopped mid-boot, as the holder's "requested" stop did.
    let port = free_port();
    let cfg = write_config(scratch.path(), &data_dir, port);
    let mut booting = Command::new(env!("CARGO_BIN_EXE_threads"))
        .arg("--config")
        .arg(&cfg)
        .env_remove("THREADS_CHASSIS_PORT")
        .env("CAULDRON_HOME", scratch.path().join("cauldron-home"))
        .env("THREADS_CLAUDE_BIN", fake_claude())
        .env("THREADS_CLAUDE_HOME", scratch.path().join("claude-home"))
        .env("HOME", scratch.path().join("home"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(10));
    unsafe {
        libc::kill(booting.id() as i32, libc::SIGTERM);
    }
    let _ = booting.wait();

    let third = Daemon::boot(scratch.path(), &data_dir).await;
    assert_eq!(
        third.changed(&disable).await,
        json!(false),
        "a mid-boot SIGTERM between two daemons must not lose {ACCOUNT}'s disable"
    );
    third.stop();
}

/// **THE OPPOSING LEG.** An enable between the two boots must ALSO survive, or
/// a daemon that pinned every account disabled forever — or answered
/// `changed: false` unconditionally — would pass the test above.
#[tokio::test(flavor = "multi_thread")]
async fn an_enable_survives_a_restart_too() {
    let scratch = tempfile::tempdir().unwrap();
    let data_dir = scratch.path().join("threads-home");
    let disable = format!("accounts/{ACCOUNT}/disable");
    let enable = format!("accounts/{ACCOUNT}/enable");

    let first = Daemon::boot(scratch.path(), &data_dir).await;
    assert_eq!(first.changed(&disable).await, json!(true));
    assert_eq!(first.changed(&enable).await, json!(true));
    first.stop();
    assert_eq!(disabled_at(&data_dir), None);

    let second = Daemon::boot(scratch.path(), &data_dir).await;
    assert_eq!(
        second.changed(&disable).await,
        json!(true),
        "an account enabled before the restart is back in the pool after it"
    );
    second.stop();
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;

/// `/health`'s `recovery.state == "done"` on `port` (R9.C7g r37).
fn recovered(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    if s.write_all(b"GET /health HTTP/1.0\r\nHost: x\r\n\r\n").is_err() {
        return false;
    }
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.split_once("\r\n\r\n")
        .and_then(|(_, b)| serde_json::from_str::<Value>(b).ok())
        .map(|h| h["recovery"]["state"] == "done")
        .unwrap_or(false)
}
