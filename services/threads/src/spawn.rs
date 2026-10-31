//! The canonical `claude` spawn recipe — ported from CCW `spawn.rs`, rebuilt
//! per-turn from live [`ThreadSettings`](crate::settings::ThreadSettings).
//!
//! CCW froze its `SpawnConfig` inside an immutable `SessionSpec`; here the
//! runtime constructs a fresh `SpawnConfig` for EVERY turn from the thread's
//! current settings + the freshly-scanned plugin set, which is what makes
//! mid-thread model changes (Q10) and plugin hot-reload (Q9) work with zero
//! extra machinery.

use crate::settings::{ThreadSettings, ToolConfig};

/// The claude binary the daemon invokes. Honors `THREADS_CLAUDE_BIN` (tests
/// point it at a fake stream-json emitter); defaults to `claude` on PATH.
pub fn claude_bin() -> String {
    std::env::var("THREADS_CLAUDE_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude".to_string())
}

/// **WHICH LOGIN A TURN SPAWNS AS** — the one thing that decides whose Claude
/// subscription, whose transcripts and whose Keychain entry a child process
/// touches.
///
/// # Why this is a TYPE, and why it replaced an `Option<&str>`
///
/// The spawn recipe used to take `Option<&str>` and document `None` as "machine
/// default". That is TRUE and it is also the defect: `None` is likewise what
/// every failure to resolve a login directory produces, and the two arrive at
/// [`SpawnConfig::env`] as the same value. `ThreadManager::config_dir_for`
/// answers `None` for any name that is not a pool entry — a renamed account, a
/// deleted one, a pool entry of some future resource type that is not a Claude
/// account — and `None` there meant `cmd.env_remove("CLAUDE_CONFIG_DIR")`,
/// which means `~/.claude`, **which is the operator's own terminal login**.
/// A turn that reached that arm by accident succeeded, silently, as him.
///
/// The safety argument that used to cover this (`config_dir_for`'s own
/// docstring: *"through the pool there is no such name"*) depends on the pool
/// being immutable for the life of the process. It is today. It stops being
/// true the moment the pool becomes reloadable, because "valid when placed"
/// then stops implying "valid when spawned".
///
/// So the two meanings are separated at the type level: an unset
/// `CLAUDE_CONFIG_DIR` is now a value a caller has to ASK FOR BY NAME
/// ([`SpawnLogin::machine_default`]), and it cannot be arrived at by an
/// `Option` that quietly went empty somewhere upstream.
///
/// # What this type does NOT do
///
/// It does not decide WHICH login a thread is entitled to — that needs the
/// thread's account name and the pool, neither of which belongs in the spawn
/// recipe. `ThreadManager::spawn_login` is the single decider and carries the
/// predicate; this type is what makes its answer impossible to fake at the
/// call site. Full unrepresentability — threading a non-optional login through
/// `ThreadState`, the store and `resolve_account` — is a much larger refactor
/// and is argued in `docs/spawn-config-dir-guard.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnLogin(Option<String>);

impl SpawnLogin {
    /// A POOL ACCOUNT's login: `CLAUDE_CONFIG_DIR` is SET to `login_dir`.
    ///
    /// `None` when `login_dir` is empty, and that refusal is not a nicety: an
    /// empty string is exactly how the registry's inline seeds spell "the
    /// machine default" (`default = ""` in `config.toml`), and exporting
    /// `CLAUDE_CONFIG_DIR=` is unset by another spelling. Accepting it would
    /// reopen the hole through a different door — the same argument
    /// [`crate::pool::ClaudeAccount::new`] makes, in the same words, because it
    /// is the same hole.
    pub fn pool_account(login_dir: &str) -> Option<Self> {
        if login_dir.is_empty() {
            return None;
        }
        Some(Self(Some(login_dir.to_string())))
    }

    /// **THE MACHINE DEFAULT: `CLAUDE_CONFIG_DIR` LEFT UNSET, DELIBERATELY.**
    ///
    /// This is a legitimate spawn and a common one — 151 of the 1,849 threads
    /// in the live store sit on the machine-default account and have always run
    /// this way (see `docs/spawn-config-dir-guard.md` for the census and for
    /// why they are left there rather than reassigned: DEC-56 forbids moving a
    /// thread's account without carrying its transcript). It is spelled out as
    /// its own constructor so that every spawn against `~/.claude` in this
    /// crate is one `grep` away, and so that reaching it requires having
    /// decided to.
    pub fn machine_default() -> Self {
        Self(None)
    }

    /// The directory to export, or `None` for the machine default.
    pub fn config_dir(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// Is this the machine default (i.e. `~/.claude`)?
    pub fn is_machine_default(&self) -> bool {
        self.0.is_none()
    }
}

/// Everything needed to construct one turn's `claude` invocation.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    /// Model alias (`fable` | `opus` | `sonnet`) or full id — verbatim.
    pub model: String,
    /// The thread/claude session uuid (identity).
    pub thread_id: String,
    /// Working directory — authoritative (Q7); set as `current_dir`.
    pub cwd: String,
    pub append_system_prompt: Option<String>,
    pub add_dirs: Vec<String>,
    /// `None` uses `--dangerously-skip-permissions` (AUI's default).
    pub permission_mode: Option<String>,
    pub tools: ToolConfig,
    /// The FULLY RESOLVED plugin dir list for THIS turn (explicit dirs +
    /// scanned roots), computed by [`crate::plugins`] just before spawn.
    pub plugin_dirs: Vec<String>,
    pub max_budget_usd: Option<f64>,
    pub fallback_model: Option<String>,
    /// `--agent <name>` (R4.S8): the opaque agent type, verbatim.
    pub agent: Option<String>,
    /// `--effort <level>` (R4.S8).
    pub effort: Option<String>,
    /// **THE TURN'S MCP SERVER CONFIG** (R4.S5): a path to a per-turn JSON
    /// file naming this daemon's own `/mcp` façade, with the run token as its
    /// bearer header. `Some` puts `--mcp-config <path>` on argv. `None` (what
    /// [`Self::from_settings`] answers) adds nothing — the runtime fills it in
    /// for every turn it runs, see [`crate::runtime`]'s `run_once`.
    pub mcp_config: Option<String>,
    /// **THE TURN'S RUN TOKEN** (R4.S5, `docs/managed-subagents.md` §5.1),
    /// exported to the child as `THREADS_RUN_TOKEN`. `None` REMOVES the
    /// variable rather than leaving it unset: a daemon that was itself started
    /// from inside a turn would otherwise hand its own parent's token down.
    pub run_token: Option<String>,
    /// **THE TURN'S STRUCTURED-OUTPUT SCHEMA** (R11.J3,
    /// `docs/STRUCTURED-OUTPUT.md` §3.2): the compact JSON of the schema the
    /// turn's delivered message carries. `Some` puts `--json-schema <schema>`
    /// on argv. `None` (what [`Self::from_settings`] answers) adds nothing — a
    /// schema is a property of a message, never of the thread's settings, so
    /// the runtime fills it per turn (`run_once`, from
    /// [`crate::managed::turn_json_schema`]) and the next turn starts without.
    pub json_schema: Option<String>,
}

/// **WHAT ONLY PRINT MODE CARRIES** — the flags [`SpawnConfig::build_args`]
/// puts on a daemon turn and [`SpawnConfig::build_interactive_args`] never
/// does. `--max-budget-usd` and `--json-schema` are here because `claude
/// --help` (2.1.x) documents them print-only; the rest ARE print mode.
pub const PRINT_ONLY_FLAGS: &[&str] = &[
    "-p",
    "--print",
    "--output-format",
    "--verbose",
    "--include-partial-messages",
    "--max-budget-usd",
    "--json-schema",
];

/// The environment variable an ATTACHED session's spawn cookie reaches the
/// interactive claude (and so its hooks) under — SH-A2. Never on argv.
pub const ATTACH_COOKIE_ENV: &str = "THREADS_ATTACH_COOKIE";

/// The environment variable an ATTACHED session's thread id reaches its hooks
/// under — SH-A2.
pub const ATTACH_THREAD_ENV: &str = "THREADS_ATTACH_THREAD";

/// The program the attach's `--settings` hooks invoke (`<bin> thread-hook
/// <Event>`, SH-A6's helper). Honors `THREADS_HOOK_BIN`; defaults to
/// `cauldron` on PATH.
pub fn hook_bin() -> String {
    std::env::var("THREADS_HOOK_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "cauldron".to_string())
}

/// The hook events an attached session reports (SHIMS.md §A.1 step 3).
pub const ATTACH_HOOK_EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"];

/// **THE `--settings` BODY FOR AN ATTACHED SESSION**: one command hook per
/// event in [`ATTACH_HOOK_EVENTS`], each `<hook_bin> thread-hook <Event>`. The
/// cookie and thread ride the ENVIRONMENT ([`ATTACH_COOKIE_ENV`],
/// [`ATTACH_THREAD_ENV`]), which hooks inherit, so neither is on any argv.
pub fn attach_settings_json(hook_bin: &str) -> serde_json::Value {
    let mut hooks = serde_json::Map::new();
    for ev in ATTACH_HOOK_EVENTS {
        hooks.insert(
            (*ev).to_string(),
            serde_json::json!([{
                "hooks": [{ "type": "command", "command": format!("{hook_bin} thread-hook {ev}") }]
            }]),
        );
    }
    serde_json::json!({ "hooks": hooks })
}

/// The environment variable a turn's run token reaches the child under.
pub const RUN_TOKEN_ENV: &str = "THREADS_RUN_TOKEN";

/// The MCP server name the daemon's façade is registered under in the child's
/// config; the model sees its tools as `mcp__threads__agents_*`.
pub const MCP_SERVER_NAME: &str = "threads";

/// **THE `--mcp-config` BODY**: one streamable-HTTP server, this daemon's own
/// `/mcp` at `url`, carrying `token` as `Authorization: Bearer`. The shape is
/// claude's `.mcp.json` shape (`mcpServers.<name>.{type,url,headers}`).
pub fn mcp_config_json(url: &str, token: &str) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            MCP_SERVER_NAME: {
                "type": "http",
                "url": url,
                "headers": { "Authorization": format!("Bearer {token}") },
            }
        }
    })
}

impl SpawnConfig {
    /// Snapshot the live settings + resolved plugin set into a spawn config.
    pub fn from_settings(thread_id: &str, s: &ThreadSettings, plugin_dirs: Vec<String>) -> Self {
        Self {
            model: s.model.clone(),
            thread_id: thread_id.to_string(),
            cwd: s.cwd.clone(),
            append_system_prompt: s.append_system_prompt.clone(),
            add_dirs: s.add_dirs.clone(),
            permission_mode: s.permission_mode.clone(),
            tools: s.tools.clone(),
            plugin_dirs,
            max_budget_usd: s.max_budget_usd,
            fallback_model: s.fallback_model.clone(),
            agent: s.agent.clone(),
            effort: s.effort.clone(),
            mcp_config: None,
            run_token: None,
            json_schema: None,
        }
    }

    /// Build the `claude` argument vector for a turn.
    ///
    /// `first_turn` selects `--session-id <uuid>` (mint) vs `--resume <uuid>`
    /// (continue). The prompt is NOT in argv — it is fed on stdin.
    pub fn build_args(&self, first_turn: bool) -> Vec<String> {
        self.build_args_for(first_turn, true)
    }

    /// **THE INTERACTIVE LAUNCH RECIPE** (SH-A2, `docs/SHIMS.md` §A.1): the
    /// argv `cauldron-claude` spawns the real TUI with, inheriting the
    /// operator's terminal.
    ///
    /// It is [`Self::build_args`] MINUS THE PRINT FLAGS — built by the same
    /// function with print mode off, never by filtering the print argv, so a
    /// flag VALUE that happens to read `-p` (a role prompt, say) cannot be
    /// mistaken for the flag. What print mode alone carries, and this does not:
    /// [`PRINT_ONLY_FLAGS`]. Everything else — model, agent, effort, role
    /// prompt, `--session-id`/`--resume`, `--add-dir`, tools, `--plugin-dir`,
    /// `--mcp-config` — is the turn's, unchanged.
    ///
    /// Plus `--settings <settings_path>`: the per-attach hooks file the daemon
    /// wrote (`0600`, beside the `--mcp-config` file), so nothing is written
    /// into the account's own settings.
    pub fn build_interactive_args(&self, first_turn: bool, settings_path: &str) -> Vec<String> {
        let mut a = self.build_args_for(first_turn, false);
        a.push("--settings".into());
        a.push(settings_path.to_string());
        a
    }

    fn build_args_for(&self, first_turn: bool, print: bool) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();

        // Streaming print mode — always for a daemon turn (the daemon parses
        // the live stream); never for an interactive attach.
        if print {
        a.push("-p".into());
        a.push("--output-format".into());
        a.push("stream-json".into());
        a.push("--verbose".into());

        // **Partial messages — the audio interface's time-to-first-word.**
        //
        // Without this the CLI emits an assistant line only when a content
        // block is COMPLETE, so the earliest anything can be spoken is full
        // generation plus one synthesis. With it, the same text also arrives
        // incrementally as `stream_event` lines wrapping the raw Anthropic
        // SSE stream — `message_start`, `content_block_start`,
        // `content_block_delta` (`text_delta` / `thinking_delta` /
        // `signature_delta`), `content_block_stop`, `message_delta`,
        // `message_stop` — under `raw.event`.
        //
        // **The complete assistant line still arrives, and it is not
        // redundant.** Observed ordering, verbatim from a capture: the CLI
        // emits ONE assistant line PER CONTENT BLOCK, after that block's last
        // delta and before its `content_block_stop`, every one carrying the
        // same `message.id`. Its text is BYTE-IDENTICAL to the concatenation
        // of that block's `text_delta`s. So anything consuming both must
        // reconcile them or say everything twice; that reconciliation is the
        // consumer's (see `translate_content_event_streaming` in AUI), and it
        // is a prefix comparison precisely because of that identity.
        //
        // The daemon itself is unchanged by this: `stream_event` already had a
        // wire type, is already `Audience::LedgerOnly` so it never re-enters
        // the model's context, and is already persisted and broadcast. Turning
        // the flag on makes those events actually happen; it does not add a
        // path.
        a.push("--include-partial-messages".into());
        }

        // Permission posture.
        match &self.permission_mode {
            Some(mode) => {
                a.push("--permission-mode".into());
                a.push(mode.clone());
            }
            None => a.push("--dangerously-skip-permissions".into()),
        }

        // Model.
        a.push("--model".into());
        a.push(self.model.clone());
        if let Some(fb) = &self.fallback_model {
            a.push("--fallback-model".into());
            a.push(fb.clone());
        }

        // Agent type and effort (R4.S8) — verbatim, each only when set.
        if let Some(ag) = &self.agent {
            a.push("--agent".into());
            a.push(ag.clone());
        }
        if let Some(e) = &self.effort {
            a.push("--effort".into());
            a.push(e.clone());
        }

        // Role prompt.
        if let Some(sp) = &self.append_system_prompt {
            a.push("--append-system-prompt".into());
            a.push(sp.clone());
        }

        // Session identity.
        if first_turn {
            a.push("--session-id".into());
        } else {
            a.push("--resume".into());
        }
        a.push(self.thread_id.clone());

        // Directory scoping.
        for d in &self.add_dirs {
            a.push("--add-dir".into());
            a.push(d.clone());
        }

        // Native per-run spend cap.
        // (print-only: `claude --help` says so; an interactive session refuses it)
        if let Some(b) = self.max_budget_usd.filter(|_| print) {
            a.push("--max-budget-usd".into());
            a.push(format!("{b}"));
        }

        // Tool clean-slate / injection (ported from CCW).
        if self.tools.clean_slate {
            a.push("--tools".into());
            a.push(String::new());
        } else if let Some(t) = &self.tools.tools {
            a.push("--tools".into());
            a.push(t.clone());
        }
        if let Some(agents) = &self.tools.agents_json {
            a.push("--agents".into());
            a.push(agents.clone());
        }
        for pd in &self.plugin_dirs {
            a.push("--plugin-dir".into());
            a.push(pd.clone());
        }
        for t in &self.tools.allowed_tools {
            a.push("--allowedTools".into());
            a.push(t.clone());
        }
        for t in &self.tools.disallowed_tools {
            a.push("--disallowedTools".into());
            a.push(t.clone());
        }

        // The daemon's own `/mcp` façade (R4.S5). A FILE path, never the JSON
        // inline: the config carries the run token, and argv is world-readable
        // through `ps`.
        if let Some(path) = &self.mcp_config {
            a.push("--mcp-config".into());
            a.push(path.clone());
        }

        // Structured output (R11.J3): only on the turn whose message asked.
        if let Some(schema) = self.json_schema.as_ref().filter(|_| print) {
            a.push("--json-schema".into());
            a.push(schema.clone());
        }

        a
    }

    /// The environment overrides for a spawn (ported from CCW, identical to
    /// AUI's): strip the API key so subscription auth is used, disable
    /// auto-memory, keep async subagents alive past claude's default 10-min
    /// ceilings. [`SpawnLogin`] selects the account.
    ///
    /// **This used to take `Option<&str>`, and that signature was the hazard.**
    /// `None` meant "leave `CLAUDE_CONFIG_DIR` unset", which means `~/.claude`
    /// — the operator's own terminal login — and NOTHING at this boundary could
    /// tell a deliberate machine-default spawn from an account whose login
    /// directory simply failed to resolve. The two are now different values of
    /// a type that has to be named out loud; see [`SpawnLogin`] and
    /// `ThreadManager::spawn_login`, which is the only thing that decides which
    /// one a turn gets.
    pub fn env(&self, login: &SpawnLogin) -> Vec<(String, Option<String>)> {
        let config_dir = login.config_dir();
        let mut e: Vec<(String, Option<String>)> = vec![
            ("ANTHROPIC_API_KEY".into(), None), // remove
            ("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), Some("1".into())),
            ("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS".into(), Some("0".into())),
            (
                "CLAUDE_ASYNC_AGENT_STALL_TIMEOUT_MS".into(),
                Some("3600000".into()),
            ),
        ];
        match config_dir {
            // A pool account: the variable is SET to the directory its login
            // lives in.
            Some(d) => e.push(("CLAUDE_CONFIG_DIR".into(), Some(d.to_string()))),
            // THE MACHINE DEFAULT, and nothing else can reach this arm any
            // more: the variable is removed, which is what `~/.claude` means.
            // The caller had to say [`SpawnLogin::machine_default`] to get
            // here, and `ThreadManager::spawn_login` is the only place in the
            // crate that says it.
            None => e.push(("CLAUDE_CONFIG_DIR".into(), None)),
        }
        // The run token (R4.S5). Removed when there is none, so a token in the
        // DAEMON's own environment can never leak into a turn it runs.
        e.push((RUN_TOKEN_ENV.into(), self.run_token.clone()));
        e
    }
}
