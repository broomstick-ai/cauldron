//! # The supervisor owns every MCP tool — the daemon's half (R5.S2, DEC-34)
//!
//! The host itself — declarations, the one awake slot, the stdio transport —
//! is `libs/mcp-host`. This module is only the daemon's binding of it: load
//! the declarations when the daemon takes its hold, and answer the four seam
//! words (`MCP_TOOLS`, `MCP_CALL`, `MCP_STATUS`, `MCP_SLEEP`) by handing them
//! to [`mcp_host::wire::answer`].
//!
//! **Who calls these.** The threads daemon's `/mcp` façade
//! (`services/threads/src/mcp_route.rs`): a model's `tools/call` for a tool
//! that is not one of threads' own in-process `agents_*` tools is forwarded
//! here as `MCP_CALL`, and this supervisor wakes the declaring server (putting
//! any other to sleep) and serves it. Threads finds this seam through the
//! `cauldron` row of `CAULDRON_SEAMS`, which the supervisor registers for
//! itself in `run` and which `launch_hosted` hands every hosted service.
//!
//! **A bad declaration does not stop supervision.** If `mcp/` fails to load
//! the daemon still hosts its services, with an EMPTY tool host, and says so
//! loudly — a typo in a tool declaration must not become a threads outage.

use anyhow::Result;
use env::warnings::Conn;
use env::Environment;
use mcp_host::{wire, ToolHost};

use super::Daemon;

/// Load `<root>/mcp/` then the shipped `mcp/` into a host. Spawns nothing.
pub(super) fn load_host(env: &Environment) -> ToolHost {
    let dirs = mcp_host::decl::search_paths(&env.root);
    let loaded = mcp_host::decl::load(&dirs).and_then(ToolHost::new);
    match loaded {
        Ok(h) => {
            let names: Vec<String> = h.decls().map(|d| d.name.clone()).collect();
            println!(
                "cauldron: holding {} MCP tool server declaration(s) [{}] from {:?}; none awake — \
                 a process server is woken on its first call, at most one at a time",
                names.len(),
                names.join(", "),
                dirs
            );
            h
        }
        Err(e) => {
            error_at!(
                "MCP tool declarations did not load ({e:#}); this supervisor holds NO MCP tools until \
                 they are fixed and it is restarted. Services are unaffected"
            );
            ToolHost::new(Vec::new()).expect("an empty host is always valid")
        }
    }
}

fn answer(tools: &ToolHost, c: &mut Conn<'_>, verb: &str, rest: &str) -> Result<()> {
    let v = wire::answer(tools, verb, rest).expect("only the four MCP words are routed here");
    c.line(&v.to_string())?;
    Ok(())
}

impl Daemon {
    pub(super) fn v_mcp_tools(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        answer(&self.tools, c, wire::TOOLS, rest)
    }
    pub(super) fn v_mcp_call(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        answer(&self.tools, c, wire::CALL, rest)
    }
    pub(super) fn v_mcp_status(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        answer(&self.tools, c, wire::STATUS, rest)
    }
    pub(super) fn v_mcp_sleep(&self, c: &mut Conn<'_>, rest: &str) -> Result<()> {
        answer(&self.tools, c, wire::SLEEP, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// The four words are on the daemon's verb table and none shadows the
    /// supervisor's own.
    #[test]
    fn the_supervisor_seam_answers_the_four_mcp_words() {
        use env::warnings::Service;
        let words: Vec<&str> = <Daemon as Service>::VERBS.iter().map(|v| v.word).collect();
        for w in wire::WORDS {
            assert!(words.contains(w), "`{w}` is not on the supervisor's verb table: {words:?}");
        }
        for ours in ["STATUS", "RESTART", "STOP"] {
            assert!(!wire::WORDS.contains(&ours));
        }
    }

    fn daemon_with(host: ToolHost) -> Daemon {
        let tmp = std::env::temp_dir();
        Daemon {
            se: env::seams::ServiceEnv {
                service: "cauldron".into(),
                env: "test".into(),
                data_root: tmp.clone(),
                data_dir: tmp.clone(),
                seams: tmp.join("seams.json"),
                boot_token: "t".into(),
                version: "v".into(),
                commit: "c".into(),
                dirty: false,
                bind: "127.0.0.1:0".into(),
            },
            addr: "127.0.0.1:0".parse().unwrap(),
            started_at: "0".into(),
            children: Arc::new(Mutex::new(BTreeMap::new())),
            asks: Arc::new(Mutex::new(Vec::new())),
            tools: Arc::new(host),
        }
    }

    fn ask(d: &Daemon, verb: &str, rest: &str) -> serde_json::Value {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut c = Conn { out: &mut out, frames: 0 };
            env::warnings::dispatch(d, verb, rest, &mut c).unwrap();
        }
        serde_json::from_slice(String::from_utf8(out).unwrap().trim().as_bytes()).unwrap()
    }

    /// **THROUGH THE DAEMON'S OWN DISPATCH**, not the library's: an
    /// `MCP_CALL` arriving on the supervisor seam wakes the declared server
    /// and is served by it; `MCP_STATUS` then names it awake, and `MCP_SLEEP`
    /// puts it down.
    #[test]
    fn an_mcp_call_on_the_supervisor_seam_wakes_and_is_served_by_the_declared_server() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../libs/mcp-host/tests/fixtures/fake-mcp-tool")
            .canonicalize()
            .unwrap();
        let decl = mcp_host::ToolDecl::parse(
            &format!(
                "name = \"alpha\"\nresidency = \"process\"\n[process]\ncommand = \"{}\"\n\
                 [process.env]\nFAKE_MCP_NAME = \"alpha\"\n[[tools]]\nname = \"alpha_echo\"\n",
                fixture.display()
            ),
            &PathBuf::from("/t/alpha.toml"),
        )
        .unwrap();
        let d = daemon_with(ToolHost::new(vec![decl]).unwrap());
        assert_eq!(ask(&d, "MCP_STATUS", "")["awake"], serde_json::Value::Null);
        let r = ask(&d, "MCP_CALL", r#"{"tool":"alpha_echo","arguments":{}}"#);
        assert_eq!(r["ok"]["structuredContent"]["served_by"], "alpha", "{r}");
        let st = ask(&d, "MCP_STATUS", "");
        assert_eq!(st["awake"]["server"], "alpha", "{st}");
        assert_eq!(ask(&d, "MCP_SLEEP", "")["slept"], "alpha");
    }
}
