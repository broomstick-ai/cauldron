//! # One spawned turn carries BOTH the pool's current login dir AND its run door
//!
//! Two fixes changed what a turn is spawned with, from two directions:
//!
//! - substrate `b1d9770` (`tests/spawn_login_guard.rs`): the turn's
//!   `CLAUDE_CONFIG_DIR` is the POOL'S login directory as of the spawn, not the
//!   one cached on `ThreadState` at placement — a re-sign-in that re-registers
//!   a slot under a new string reaches the next turn.
//! - R4.S5 (`tests/turn_mcp_door.rs`): every turn gets its own run token in
//!   `THREADS_RUN_TOKEN` and `--mcp-config <file>` naming this daemon's bound
//!   `/mcp` with that token as its bearer.
//!
//! Each has its own test, and each of those tests would stay green if the
//! other fix were lost at the spawn site (the login test uses the plain
//! `fake-claude`, which never looks at the token; the door test never moves a
//! login directory). This file asserts both on the SAME spawned process.
//!
//! ## A probe must not witness itself
//!
//! Nothing here mints a token or chooses the login directory the child sees.
//! The wrapper the daemon spawns records the `CLAUDE_CONFIG_DIR` it was handed
//! and then `exec`s `fake-claude-capture`, which records the token, argv and
//! the `--mcp-config` file. Every value asserted on was put there by the
//! daemon's spawn path. The old and new directories are asserted distinct
//! before anything else, so "the child saw the new one" cannot be `x == x`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use threads::config::Config;
use threads::build_manager;

const INHERITED: &str = "inherited-from-the-daemons-env";

fn capture_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude-capture"))
}

/// The binary the daemon spawns: record `CLAUDE_CONFIG_DIR` (or `<unset>`),
/// then become `fake-claude-capture` with argv and stdin untouched.
fn write_wrapper(tmp: &Path, login_out: &Path) -> PathBuf {
    let w = tmp.join("claude-wrapper");
    std::fs::write(
        &w,
        format!(
            "#!/usr/bin/env bash\nprintf '%s' \"${{CLAUDE_CONFIG_DIR-<unset>}}\" > '{}'\nexec '{}' \"$@\"\n",
            login_out.display(),
            capture_fixture().display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&w, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    w
}

/// **A pool reload that moves an account's login directory, then one turn:
/// that turn's process holds the NEW directory, a daemon-minted run token, and
/// an `--mcp-config` naming the bound `/mcp` with that same token.**
///
/// Forced red (R3.S2.sync5 report): (1) `run_once` reads the cached
/// `state.config_dir` instead of `current_config_dir` → the child sees the old
/// directory; (2) `run_once` sets `cfg.run_token = None` → the child holds no
/// token and the config's bearer matches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_login_dir_and_the_run_token_reach_the_same_spawned_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let login_out = tmp.path().join("child-claude-config-dir");
    std::env::set_var("CAULDRON_HOME", tmp.path().join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", write_wrapper(tmp.path(), &login_out));
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    // A token in the daemon's own environment must never be what a turn holds.
    std::env::set_var("THREADS_RUN_TOKEN", INHERITED);
    threads::clock::testing::ticking();

    let old = tmp.path().join("aa-moved");
    let new = tmp.path().join("aa-moved-signed-in");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    let old_s = old.to_string_lossy().to_string();
    let new_s = new.to_string_lossy().to_string();
    assert_ne!(old_s, new_s, "sentinel: the two login directories are different strings");

    let mut accounts = BTreeMap::new();
    accounts.insert("aa-moved".to_string(), old_s.clone());
    let m = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let (addr, h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    std::mem::forget(h);

    let settings = json!({
        "cwd": tmp.path().to_string_lossy(),
        "model": "sonnet",
        "account": {"named": "aa-moved"},
    });
    let id = m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "aa-moved");
    assert_eq!(m.pool().login_dir("aa-moved"), Some(old_s.as_str()), "placed on the OLD directory");

    // The re-sign-in rewrites the registry; the daemon reloads the pool.
    let reg = tmp.path().join("accounts.toml");
    let toml = std::fs::read_to_string(&reg).unwrap();
    let quoted_old = format!("\"{old_s}\"");
    assert!(toml.contains(&quoted_old), "registry names the old dir: {toml}");
    std::fs::write(&reg, toml.replace(&quoted_old, &format!("\"{new_s}\""))).unwrap();
    assert!(m.reload_pool(), "the pool must see the moved login directory");
    assert_eq!(m.pool().login_dir("aa-moved"), Some(new_s.as_str()));

    // One turn, held open until the capture is complete.
    let cap = tmp.path().join("cap");
    let rel = tmp.path().join("release");
    m.send(
        &id,
        format!("capture={} release={}", cap.display(), rel.display()),
        threads::SendIntent::Immediate,
        Value::Null,
        false,
    )
    .unwrap();
    let done = PathBuf::from(format!("{}.done", cap.display()));
    // R9.F2: the daemon execs `claude-wrapper`, a script THIS test wrote a
    // moment ago, so its first exec waits on the machine's per-file assessment
    // (measured 5.5–22 s under suite load; red here at exactly 20.08 s in the
    // R9.F2 baseline). The wrapper cannot be pre-run — running it is the probe —
    // so the wait is the old 20 s for the turn PLUS the first-exec budget.
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(20) + first_exec::FIRST_EXEC_BUDGET;
    while !done.exists() {
        assert!(tokio::time::Instant::now() < deadline, "the turn never ran the fixture");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let p = |ext: &str| PathBuf::from(format!("{}.{ext}", cap.display()));
    let seen_login = std::fs::read_to_string(&login_out).expect("the wrapper recorded CLAUDE_CONFIG_DIR");
    let token = std::fs::read_to_string(p("token")).ok();
    let argv: Vec<String> = std::fs::read_to_string(p("argv")).unwrap().lines().map(str::to_string).collect();
    let mcp: Option<Value> = std::fs::read_to_string(p("mcp")).ok().map(|s| serde_json::from_str(&s).unwrap());

    // Release the turn before asserting, so a red does not leave it hanging.
    std::fs::write(&rel, b"").unwrap();

    // 1. The login: the pool's CURRENT directory, not the one cached at placement.
    assert_eq!(
        seen_login, new_s,
        "the spawned turn's CLAUDE_CONFIG_DIR must be the pool's current login dir (old was {old_s})"
    );

    // 2. The run token: present, daemon-minted, not inherited.
    let token = token.expect("THREADS_RUN_TOKEN reached the same spawned turn");
    assert_ne!(token, INHERITED, "the daemon's own env leaked into the turn");
    assert!(token.starts_with("rt1."), "a run token: {token}");

    // 3. The MCP config: on argv, naming the bound /mcp, bearing THAT token.
    assert!(argv.iter().any(|a| a == "--mcp-config"), "--mcp-config on argv: {argv:?}");
    let cfg = mcp.unwrap_or_else(|| panic!("the --mcp-config file existed while the turn ran: {argv:?}"));
    let s = &cfg["mcpServers"]["threads"];
    assert_eq!(s["url"], json!(format!("http://{addr}/mcp")), "{cfg}");
    assert_eq!(s["headers"]["Authorization"], json!(format!("Bearer {token}")), "{cfg}");
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
