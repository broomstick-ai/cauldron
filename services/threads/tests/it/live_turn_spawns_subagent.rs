//! # LIVE PROOF (R4.S5): a REAL model turn spawns a subagent the daemon owns
//!
//! `#[ignore]`d, and never part of `cargo test`: it runs the REAL `claude`
//! binary (whatever `claude` resolves to on `PATH`, exactly as the daemon
//! resolves it in prod when `THREADS_CLAUDE_BIN` is unset) against a real
//! login, and costs a real model turn. Run it by hand:
//!
//! ```text
//! THREADS_LIVE_PROOF_ACCOUNT_DIR=<a logged-in claude config dir> \
//!   cargo test -p threads --test it -- --ignored --nocapture live_turn_spawns_subagent::
//! ```
//!
//! Everything else is throwaway: a temp `THREADS_HOME`/data dir, a temp
//! `CAULDRON_HOME`, an ephemeral port named to the daemon ONLY through
//! `THREADS_CHASSIS_PORT` (the config still says 3653), and the daemon is stood
//! up the way `run_daemon` stands it up — listener bound first, served through
//! `chassis::chassis` — so the `/mcp` URL the turn is handed comes from the
//! same fallback prod uses. **Nothing here mints a token**: the only token in
//! play is the one the daemon put into the turn's environment and config.
//!
//! What it shows, read off the daemon's own store and ledgers afterwards: the
//! parent turn called `agents_spawn` through the MCP tool; a child thread
//! exists in the store; its managed ledger (written by the daemon) names the
//! parent; the parent's ledger records the child as started.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use threads::config::Config;
use threads::build_manager;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "LIVE: runs the real claude binary against a real login; run by hand"]
async fn a_real_turn_spawns_a_subagent_that_the_daemon_owns() {
    let account_dir = std::env::var("THREADS_LIVE_PROOF_ACCOUNT_DIR")
        .expect("set THREADS_LIVE_PROOF_ACCOUNT_DIR to a logged-in claude config dir");
    assert!(Path::new(&account_dir).is_dir(), "{account_dir} is a directory");
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("threads-home");
    std::fs::create_dir_all(&data).unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::env::set_var("THREADS_HOME", &data);
    std::env::set_var("CAULDRON_HOME", tmp.path().join("cauldron"));
    std::env::remove_var("THREADS_CLAUDE_BIN");
    std::env::remove_var("THREADS_RUN_TOKEN");
    std::env::set_var("THREADS_UPDATE_WATCH", "off");

    // The run_daemon shape: bind first, name the port ONLY via the env.
    let listener = threads::server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    assert_ne!(port, 3653, "never the live port");
    std::env::set_var("THREADS_CHASSIS_PORT", port.to_string());
    let mut accounts = BTreeMap::new();
    accounts.insert("live-proof".to_string(), account_dir.clone());
    let cfg = Config {
        data_dir: Some(data.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        ..Default::default()
    };
    let m = build_manager(cfg).unwrap();
    let (bound, _h) = threads::chassis::chassis(Arc::new(m.clone()))
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    eprintln!("[live] throwaway daemon on {bound}, data {}", data.display());

    let settings = json!({
        "cwd": work.to_string_lossy(),
        "model": "sonnet",
        "account": {"named": "live-proof"},
    });
    let parent = m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap();
    eprintln!("[live] parent thread {parent}");
    let prompt = "You have an MCP server named `threads`. Call its `agents_spawn` tool exactly once, \
                  with task \"Reply with the single word: pong\" and request_id \"live-proof-1\". \
                  Do not use any other tool. Then reply with the thread_id it returned, and nothing else.";
    m.send(&parent, prompt.to_string(), threads::SendIntent::Immediate, Value::Null, false).unwrap();

    // Wait for the parent's turn to start and then to end.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while m.store.get_running_turn(&parent).unwrap().is_none() {
        assert!(tokio::time::Instant::now() < deadline, "the parent's turn never started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let cfg_file = std::fs::read_dir(data.join("turns").join(&parent))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.to_string_lossy().ends_with(".mcp.json"))
        .expect("the daemon wrote this turn's --mcp-config under its own data dir");
    let body: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_file).unwrap()).unwrap();
    let url = body["mcpServers"]["threads"]["url"].as_str().unwrap().to_string();
    eprintln!("[live] turn's --mcp-config {} names {url}", cfg_file.display());
    assert_eq!(url, format!("http://127.0.0.1:{port}/mcp"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    while m.store.get_running_turn(&parent).unwrap().is_some() {
        assert!(tokio::time::Instant::now() < deadline, "the parent's turn never ended");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(!cfg_file.exists(), "the per-turn config is gone at turn end");

    // What the real claude printed this turn (its durable stdout), for the record.
    // FIRST PROOF, independent of the model doing anything: the real binary's
    // `init` line reports the daemon's `threads` MCP server CONNECTED and its
    // `agents_*` tools listed — i.e. claude read the --mcp-config the daemon
    // wrote, dialled the URL in it with the daemon-minted bearer, and `/mcp`
    // accepted it (an unauthorized server lists no tools).
    let mut connected = false;
    for e in std::fs::read_dir(data.join("turns").join(&parent)).unwrap().filter_map(|e| e.ok()) {
        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
        for line in text.lines() {
            let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
            let t = v["type"].as_str().unwrap_or("");
            if t == "system" && v["subtype"] == "init" {
                let servers = v["mcp_servers"].to_string();
                let spawn_listed = v["tools"].as_array().map_or(false, |a| a.iter().any(|x| x == "mcp__threads__agents_spawn"));
                eprintln!("[live] init: mcp_servers={servers} agents_spawn_listed={spawn_listed}");
                let threads_up = v["mcp_servers"].as_array().map_or(false, |a| {
                    a.iter().any(|x| x["name"] == "threads" && x["status"] == "connected")
                });
                connected |= threads_up && spawn_listed;
            }
            if t == "assistant" || t == "user" || t == "result" || (t == "system" && v["subtype"] == "init") {
                let mut l = line.to_string();
                l.truncate(1200);
                eprintln!("[live] stdout {}: {l}", e.file_name().to_string_lossy());
            }
        }
    }

    assert!(connected, "the real claude connected to the daemon's /mcp with the daemon's token");

    // The child: a ledger the DAEMON wrote, naming the parent.
    let ledgers = data.join("ledgers");
    let children: Vec<String> = std::fs::read_dir(&ledgers)
        .unwrap_or_else(|e| panic!("no ledgers dir at {}: the turn spawned nothing ({e})", ledgers.display()))
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".jsonl")).map(str::to_string))
        .filter(|id| id != &parent)
        .collect();
    eprintln!("[live] ledgers: {children:?}");
    assert_eq!(children.len(), 1, "exactly one child was spawned: {children:?}");
    let child = &children[0];
    let child_ledger = std::fs::read_to_string(ledgers.join(format!("{child}.jsonl"))).unwrap();
    eprintln!("[live] child ledger:\n{child_ledger}");
    assert!(child_ledger.contains(&parent), "the child's ledger names its parent");
    let parent_ledger = std::fs::read_to_string(ledgers.join(format!("{parent}.jsonl"))).unwrap();
    eprintln!("[live] parent ledger:\n{parent_ledger}");
    assert!(parent_ledger.contains(child.as_str()), "the parent's ledger records the child");
    let row = m.store.get_thread(child).unwrap().expect("the child is a thread in the daemon's store");
    eprintln!("[live] child row: account={:?} cwd={:?}", row.account, row.cwd);

    // Let the child's own turn finish so nothing is left running.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if m.store.get_running_turn(child).unwrap().is_none() && m.store.get_running_turn(&parent).unwrap().is_none() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "the child's turn never ended");
    }
    let final_child = std::fs::read_to_string(ledgers.join(format!("{child}.jsonl"))).unwrap();
    eprintln!("[live] child ledger at end:\n{final_child}");
    std::env::remove_var("THREADS_CHASSIS_PORT");
}
