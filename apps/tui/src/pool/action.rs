//! The board's closed set of acts, and exactly what each one sends.
//!
//! Every act is decided here, once, as a [`Command`]: a WebSocket request to
//! the threads daemon (method + params), or the one LOCAL carve-out the
//! Python board also has — setting the machine's login, which writes
//! `~/.claude.json` and the keychain through the `claude-accounts` tool and
//! which the daemon only ever reads back (threads `scripts/threads-app/
//! threads_tui/local.py`). The tests pin this table; the event loop only
//! executes what it returns.

use std::path::PathBuf;

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Disable(String),
    Enable(String),
    /// `s`: make this account the machine's live login.
    SetLive(String),
    /// `S`: the account's pool share, 0 < share ≤ 1. `None` clears the
    /// override.
    SetShare(String, Option<f64>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Ws { method: String, params: Value },
    Local { argv: Vec<String> },
}

impl Action {
    pub fn label(&self) -> &str {
        match self {
            Action::Disable(l) | Action::Enable(l) | Action::SetLive(l) | Action::SetShare(l, _) => l,
        }
    }

    /// What the confirm line asks.
    pub fn question(&self) -> String {
        match self {
            Action::Disable(l) => format!("disable {l}? it leaves the pool and gets no new work"),
            Action::Enable(l) => format!("reactivate {l}? it rejoins the pool"),
            Action::SetLive(l) => format!("make {l} this machine's login? (rewrites ~/.claude.json + keychain)"),
            Action::SetShare(l, Some(s)) => format!("set {l}'s pool share to {:.0}%?", s * 100.0),
            Action::SetShare(l, None) => format!("clear {l}'s pool share override?"),
        }
    }

    pub fn command(&self) -> Command {
        match self {
            Action::Disable(l) => Command::Ws { method: format!("accounts/{l}/disable"), params: json!({}) },
            Action::Enable(l) => Command::Ws { method: format!("accounts/{l}/enable"), params: json!({}) },
            Action::SetShare(l, s) => Command::Ws {
                method: format!("accounts/{l}/pool_share_set"),
                params: json!({ "share": s }),
            },
            Action::SetLive(l) => Command::Local { argv: set_live_argv(l, Mode::Preflight) },
        }
    }
}

/// The three ways `set-live-login` is run, as in `local.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `--dry-run`: what it WOULD do. Always first.
    Preflight,
    /// No flag, stdin closed: records the name when the preflight said the
    /// machine login already IS this account (stops at the same branch).
    RecordAlreadyLive,
    /// `--yes`, on the operator's terminal: the real switch, which may open a
    /// browser sign-in.
    Switch,
}

pub const ALREADY_LIVE_MARKER: &str = "already-live:";
pub const DRY_RUN_MARKER: &str = "--dry-run: stopping here";

/// The `claude-accounts` bash tool. `TUI_POOL_ACCOUNTS_TOOL` overrides it
/// (tests point it at a stub; nothing in a test may touch the real machine
/// login). DEPTH-DEPENDENT default: this crate lives two directories down.
pub fn accounts_tool() -> PathBuf {
    if let Some(p) = std::env::var_os("TUI_POOL_ACCOUNTS_TOOL") {
        return PathBuf::from(p);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/threads/scripts/claude-accounts")
}

pub fn set_live_argv(label: &str, mode: Mode) -> Vec<String> {
    let mut v = vec![
        accounts_tool().to_string_lossy().into_owned(),
        "set-live-login".to_string(),
        label.to_string(),
    ];
    match mode {
        Mode::Preflight => v.push("--dry-run".into()),
        Mode::RecordAlreadyLive => {}
        Mode::Switch => v.push("--yes".into()),
    }
    v
}

/// Refuse while `CLAUDE_CONFIG_DIR` is set at all: the child would write a
/// slot instead of the machine store (`local.py::config_dir_refusal`).
pub fn config_dir_refusal() -> Option<String> {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(|v| {
        format!(
            "CLAUDE_CONFIG_DIR is set ({}) — setting the machine login from here would write the wrong store",
            if v.is_empty() { "(empty)".into() } else { v.to_string_lossy().into_owned() }
        )
    })
}

/// What the preflight's output says to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    AlreadyLive,
    Proceed,
    Refuse(String),
}

pub fn read_preflight(ok: bool, stdout: &str, stderr: &str) -> Preflight {
    if !ok {
        let tail = stderr.lines().chain(stdout.lines()).filter(|l| !l.trim().is_empty()).last();
        return Preflight::Refuse(tail.unwrap_or("the preflight failed").to_string());
    }
    if stdout.contains(ALREADY_LIVE_MARKER) {
        return Preflight::AlreadyLive;
    }
    if !stdout.contains(DRY_RUN_MARKER) {
        return Preflight::Refuse(
            "the preflight exited cleanly but never reached its dry-run stop — refusing".into(),
        );
    }
    Preflight::Proceed
}
