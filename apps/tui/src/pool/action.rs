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
        // Drawn: the account's name, in full (#35).
        self.question_as(self.label())
    }

    /// [`Action::question`] naming the account as `shown` — the caller's
    /// drawn name for it (one that tells twins apart, V1-ACCT). The label is
    /// never re-masked here, so a name is masked exactly once.
    pub fn question_as(&self, shown: &str) -> String {
        let l = shown;
        match self {
            Action::Disable(_) => format!("disable {l}? it leaves the pool and gets no new work"),
            Action::Enable(_) => format!("reactivate {l}? it rejoins the pool"),
            Action::SetLive(_) => format!("make {l} this machine's login? (rewrites ~/.claude.json + keychain)"),
            Action::SetShare(_, Some(s)) => format!("set {l}'s pool share to {:.0}%?", s * 100.0),
            Action::SetShare(_, None) => format!("clear {l}'s pool share override?"),
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

/// The `claude-accounts` bash tool this process would run, BEFORE any check:
/// `TUI_POOL_ACCOUNTS_TOOL` when it is set (tests point it at a stub; nothing
/// in a test may touch the real machine login), else the default of this
/// build ([`default_accounts_tool`]), else `None`. What may actually run is
/// decided by [`local_refusal`].
pub fn accounts_tool() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TUI_POOL_ACCOUNTS_TOOL").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    default_accounts_tool()
}

/// Where the helper is installed, relative to the cauldron root (the
/// directory above this binary's `bin/`): `scripts/install` lays it down there
/// on the prebuilt and the from-source path alike.
pub const INSTALLED_HELPER: &str = "share/cauldron/scripts/threads/claude-accounts";

/// **THE INSTALLED HELPER** (issue #30): `<root>/share/cauldron/scripts/threads/claude-accounts`,
/// where `<root>` is the directory above the one this executable sits in,
/// every link resolved (the installed `tui` is `<root>/bin/tui`). Read at run
/// time, never compiled in; `None` when there is no file there. It is still
/// only run when [`local_refusal`] admits it.
pub fn installed_accounts_tool() -> Option<PathBuf> {
    installed_tool_for(&std::env::current_exe().ok()?)
}

/// [`installed_accounts_tool`] for the executable `exe`.
pub fn installed_tool_for(exe: &std::path::Path) -> Option<PathBuf> {
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let p = exe.parent()?.parent()?.join(INSTALLED_HELPER);
    p.is_file().then_some(p)
}

/// **A RELEASE-BUNDLE `tui` COMPILES IN NO HELPER PATH.** It was compiled in
/// a scratch directory that does not exist on the machine it runs on, and a
/// path compiled from it (`/private/tmp/...` on the build machine) is one any
/// local user could create. Its default is the helper installed beside it
/// ([`installed_accounts_tool`]), or none.
#[cfg(cauldron_release_bundle)]
pub fn default_accounts_tool() -> Option<PathBuf> {
    installed_accounts_tool()
}

/// A `tui` built from a checkout runs the installed helper when it is
/// installed ([`installed_accounts_tool`]), else that checkout's
/// `services/threads/scripts/claude-accounts`. DEPTH-DEPENDENT: this crate
/// lives two directories down. Either is only run when [`local_refusal`]
/// admits it: owned by you, in directories only you or root can write, so a
/// checkout built in a shared directory is refused rather than trusted.
#[cfg(not(cauldron_release_bundle))]
pub fn default_accounts_tool() -> Option<PathBuf> {
    installed_accounts_tool()
        .or_else(|| Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../services/threads/scripts/claude-accounts")))
}

/// The threads store the helper is told to use (`CLAUDE_ACCOUNTS_HOME`): the
/// environment's `<data root>/threads/store`, the daemon's `THREADS_HOME` as
/// the shipped declaration sets it. `None` when the environment does not
/// resolve or the store is not there (an installed helper then refuses,
/// saying the store must be named).
pub fn accounts_store() -> Option<PathBuf> {
    let env = env::Environment::resolve().ok()?;
    let store = env.service_data("threads").ok()?.join("store");
    store.is_dir().then_some(store)
}

/// The environment a helper run gets on top of this process's: the store.
pub fn helper_env() -> Vec<(&'static str, PathBuf)> {
    accounts_store().map(|s| vec![("CLAUDE_ACCOUNTS_HOME", s)]).unwrap_or_default()
}

/// **How the helper is started** (the preflight and the terminal switch
/// alike): `argv` from the accounts store (or `/` when there is none), never
/// the directory `tui` was started from, so nothing planted in a project the
/// board was opened in is found; with no variable that would make the script
/// or its python load a file the caller chose, and a PATH of absolute entries
/// only (`env::trust::helper_command`); plus [`helper_env`].
pub fn helper_command(argv: &[String]) -> std::process::Command {
    let dir = accounts_store().unwrap_or_else(|| PathBuf::from("/"));
    let mut cmd = env::trust::helper_command(argv[0].as_ref(), &dir);
    cmd.args(&argv[1..]).envs(helper_env());
    cmd
}

/// What `tui` says when there is no helper to run.
pub const NO_HELPER_IN_THIS_BUILD: &str = "set-live-login needs the claude-accounts helper, which is installed \
     with cauldron at <cauldron root>/share/cauldron/scripts/threads/claude-accounts and is not there. Install or \
     upgrade with the release bundle's `install --prebuilt <tarball> --replace-running` or `scripts/install \
     --replace-running` (a clone), or start the board with TUI_POOL_ACCOUNTS_TOOL naming a copy (a file you own, \
     in directories only you or root can write to).";

/// **REL-6a5: AN ISOLATED PROCESS NEVER RUNS THE REAL `claude-accounts`.**
/// `set-live-login` switches the machine's Claude login, so under isolation
/// (`cauldron::live::isolation`) the tool must be pointed explicitly at a
/// stub IN A SCRATCH DIR with `TUI_POOL_ACCOUNTS_TOOL`; unpointed (the default
/// is under his home) or pointed under his home is refused, nothing runs.
/// Since MERGE-15 this is [`isolated_set_live_refusal`] on the configured tool
/// — the same choke point, not a second rule.
pub fn isolation_refusal() -> Option<String> {
    isolated_set_live_refusal(&accounts_tool().unwrap_or_default())
}

/// **EVERY LOCAL RUN PASSES HERE** (the preflight and the terminal switch
/// alike): the isolation rule first, then whether `tool` may be executed at
/// all ([`env::trust::trusted_executable`]: an absolute path to a regular
/// file you own, not writable by group or others, in directories owned by
/// you or root that nobody else can write). An empty `tool` is the release
/// bundle's "no helper" ([`NO_HELPER_IN_THIS_BUILD`]). `None` = run it.
pub fn local_refusal(tool: &std::path::Path) -> Option<String> {
    if let Some(why) = isolated_set_live_refusal(tool) {
        return Some(why);
    }
    helper_refusal(tool)
}

/// [`local_refusal`] without the isolation rule.
pub fn helper_refusal(tool: &std::path::Path) -> Option<String> {
    if tool.as_os_str().is_empty() {
        return Some(NO_HELPER_IN_THIS_BUILD.to_string());
    }
    env::trust::trusted_executable(tool, false)
        .err()
        .map(|why| format!("refused to run the claude-accounts helper: {why}. Nothing ran."))
}

pub fn set_live_argv(label: &str, mode: Mode) -> Vec<String> {
    let mut v = vec![
        accounts_tool().unwrap_or_default().to_string_lossy().into_owned(),
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

/// **AN ISOLATED PROCESS NEVER RUNS HIS `claude-accounts`** (REL-6a4b,
/// REL-6a4-I N2). `set-live-login` writes `~/.claude.json` and the keychain
/// under whatever HOME it inherits, and the keychain is not HOME-scoped at
/// all, so no scratch HOME isolates it. In an isolated process (a test, a
/// `--dump`, the marker) the child runs only when `tool` is itself an
/// isolated root — a stub a test planted in its own scratch — and never when
/// it is the default (in this repo, under his home) or any copy under his
/// home. `None` for his interactive board.
///
/// **ONE REFUSAL PATH** (MERGE-15): REL-6a4b's local refusal is folded into
/// REL-6a5's egress choke point, [`env::egress::admit_login_tool`]; this and
/// [`isolation_refusal`] only supply the tool and the protected homes.
/// `tool` counts as POINTED only when it is the `TUI_POOL_ACCOUNTS_TOOL` this
/// process was given; anything else (the default) reaches the choke point as
/// unpointed and is refused there.
pub fn isolated_set_live_refusal(tool: &std::path::Path) -> Option<String> {
    let why = crate::cauldron::live::isolation(false)?;
    let pointed = std::env::var_os("TUI_POOL_ACCOUNTS_TOOL").filter(|v| !v.is_empty()).is_some_and(|v| std::path::Path::new(&v) == tool);
    env::egress::admit_login_tool("set-live-login", pointed.then_some(tool), &crate::cauldron::live::protected_homes(), Some(&why)).err()
}

/// [`isolated_set_live_refusal`] over explicit inputs (`tool` given = pointed).
pub fn isolated_set_live_refusal_from(isolated: Option<&str>, tool: &std::path::Path, protected: &[PathBuf]) -> Option<String> {
    env::egress::admit_login_tool("set-live-login", Some(tool), protected, isolated).err()
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
