//! # The two halves of the supervisor's MCP wire, joined — R5.S2b (DEC-34)
//!
//! The wire is spelled twice: the supervisor's half in
//! `libs/mcp-host/src/wire.rs`, and this daemon's half, by hand, in
//! `src/mcp_route.rs` (threads links no workspace crate but the chassis). The
//! R5.S2 inspector found that NO test joined them: `mcp_supervisor_route.rs`
//! talks to a hand-written fake supervisor, and the host's own tests never
//! leave the host. A drift on either side stayed green on both.
//!
//! This test runs threads' REAL client (`mcp_route::declared_tools`,
//! `mcp_route::call`) against the supervisor's REAL `wire::answer` over a TCP
//! socket, with a real `ToolHost` waking the real stdio fixture. `mcp-host` is
//! a DEV-dependency only, so the no-workspace-crates rule for the threads
//! library is unchanged.
//!
//! Forcings (red text in the R5.S2b report): the supervisor's half renaming
//! `unknown_tool` → red on the unknown-name mapping; threads' half reading a
//! renamed `tools` array → red on listing.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mcp_host::decl::ToolDecl;
use mcp_host::host::{Bounds, ToolHost};
use serde_json::json;
use shapewire::envelope::codes;
use shapewire::token::Claims;

fn fixture() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../libs/mcp-host/tests/fixtures/fake-mcp-tool")
        .canonicalize()
        .unwrap()
        .display()
        .to_string()
}

/// Each tool carries a NON-DEFAULT `input_schema`, so a dropped or renamed
/// `inputSchema` on either half cannot pass as `default == default` (R5.S2b
/// inspect finding 2: threads falls back to `{"type":"object"}`, which is also
/// the host's default for a tool that declares none).
fn process_decl(name: &str, tool: &str, extra_env: &str) -> ToolDecl {
    ToolDecl::parse(
        &format!(
            "name = \"{name}\"\nresidency = \"process\"\n[process]\ncommand = \"{}\"\n\
             [process.env]\nFAKE_MCP_NAME = \"{name}\"\n{extra_env}\
             [[tools]]\nname = \"{tool}\"\ndescription = \"cross tool {tool}\"\n\
             input_schema = {{ type = \"object\", properties = {{ {tool}_arg = {{ type = \"string\" }} }}, required = [\"{tool}_arg\"] }}\n",
            fixture()
        ),
        &PathBuf::from(format!("/t/{name}.toml")),
    )
    .unwrap()
}

/// A supervisor seam that answers with the REAL `wire::answer`, one thread
/// per connection exactly as the cauldron daemon serves its seam.
fn serve(host: Arc<ToolHost>) -> std::net::SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let host = host.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(s.try_clone().unwrap()).read_line(&mut line).is_err() {
                    return;
                }
                let line = line.trim_end();
                let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
                let answer = mcp_host::wire::answer(&host, verb, rest)
                    .unwrap_or_else(|| json!({"refused": {"code": "unknown_word", "message": verb}}));
                let _ = writeln!(s, "{answer}");
            });
        }
    });
    addr
}

fn caller() -> Claims {
    Claims { thread_id: "t-cross".into(), run_id: "r1".into(), root: "t-cross".into(), depth: 0, turn: "c".into() }
}

/// One test, because it sets the process-wide `CAULDRON_SUPERVISOR_ADDR`.
#[tokio::test(flavor = "multi_thread")]
async fn threads_client_and_supervisor_wire_agree_end_to_end_and_listing_outlasts_a_wedged_tool_past_the_400ms_preempt_bound() {
    let in_process = ToolDecl::parse(
        "name = \"agents\"\nresidency = \"in-process\"\nprovider = \"threads\"\n[[tools]]\nname = \"agents_spawn\"\n",
        &PathBuf::from("/t/agents.toml"),
    )
    .unwrap();
    let host = Arc::new(
        ToolHost::with_bounds(
            vec![
                in_process,
                process_decl("alpha", "alpha_echo", ""),
                process_decl("stuck", "stuck_echo", "FAKE_MCP_HANG = \"tools/call\"\n"),
            ],
            Bounds { handshake: Duration::from_secs(60), call: Duration::from_secs(60), preempt: Duration::from_millis(400) },
        )
        .unwrap(),
    );
    let addr = serve(host.clone());
    std::env::set_var(threads::mcp_route::SUPERVISOR_ADDR_VAR, addr.to_string());

    // LISTING: the supervisor's process tools reach threads with their shape.
    let own = threads::agents::defs();
    let listed = threads::mcp_route::declared_tools(&own);
    let names: Vec<&str> = listed.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["alpha_echo", "stuck_echo"], "the supervisor's process tools, and not its in-process ones");
    // THE FULL DESCRIPTOR, every field, against what the declaration says.
    for t in &listed {
        let n = t.name.as_str();
        assert_eq!(t.description, format!("cross tool {n}"), "description of {n}");
        let want = json!({"type": "object", "properties": {format!("{n}_arg"): {"type": "string"}}, "required": [format!("{n}_arg")]});
        assert_ne!(want, json!({"type": "object"}), "sentinel: the expected schema is not the fallback");
        assert_eq!(t.input_schema, want, "inputSchema of {n} did not cross the wire intact");
    }

    // CALL: served by the woken process, structured content as the payload.
    let ok = threads::mcp_route::call(&caller(), "alpha_echo", json!({"x": 1})).await.expect("alpha_echo served");
    assert_eq!(ok["served_by"], "alpha", "{ok}");

    // UNKNOWN NAME: the supervisor's `unknown_tool` is the door's -32602.
    let unk = threads::mcp_route::call(&caller(), "nope", json!({})).await.unwrap_err();
    assert_eq!(unk.code, codes::UNKNOWN_METHOD, "{}", unk.message);

    // IN-PROCESS NAME sent to the supervisor (threads never does; the wire
    // still answers it typed): an error naming the provider.
    let ip = threads::mcp_route::call(&caller(), "agents_spawn", json!({})).await.unwrap_err();
    assert_eq!(ip.code, codes::ERROR);
    assert!(ip.message.contains("in_process"), "{}", ip.message);

    // A WEDGED TOOL: threads' listing still answers at once, another tool's
    // call is served after the preempt bound, and the wedged call comes back
    // an error for that tool only.
    let wedged = tokio::spawn(async { threads::mcp_route::call(&caller(), "stuck_echo", json!({})).await });
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.awake().map(|a| a.0) != Some("stuck".into()) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let t0 = Instant::now();
    let listed = tokio::task::spawn_blocking(move || threads::mcp_route::declared_tools(&own)).await.unwrap();
    assert_eq!(listed.len(), 2, "threads' listing dropped the supervisor's tools while one is wedged");
    assert!(t0.elapsed() < Duration::from_millis(400), "threads' listing waited {:?} on a wedged tool", t0.elapsed());
    let t0 = Instant::now();
    let ok = tokio::time::timeout(Duration::from_secs(3), threads::mcp_route::call(&caller(), "alpha_echo", json!({})))
        .await
        .expect("alpha_echo's call did not complete within 3s while stuck_echo is wedged")
        .expect("alpha_echo served");
    assert_eq!(ok["served_by"], "alpha");
    assert!(t0.elapsed() >= Duration::from_millis(400), "served before the preempt bound: {:?}", t0.elapsed());
    let w = tokio::time::timeout(Duration::from_secs(2), wedged).await.expect("the wedged call was never answered").unwrap();
    let e = w.unwrap_err();
    assert_eq!(e.code, codes::ERROR);
    assert!(e.message.contains("tool_failed") && e.message.contains("put to sleep"), "{}", e.message);
    host.sleep();
}
