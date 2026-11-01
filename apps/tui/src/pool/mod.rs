//! `tui pool`: the threads account-pool board, rebuilt in the cauldron TUI
//! framework (R7.S2, DEC-37). It replaces the Python board at
//! `services/threads/scripts/threads-app` (`threads_tui`), which stays in the
//! tree untouched.
//!
//! `model` folds the daemon's `accounts` and `threads/non-idle` shapes and
//! projects them into cards; `action` is the closed set of acts and exactly
//! what each sends; `app` holds state and keys with no terminal; `view` draws
//! a frame; `client` is the one WebSocket to the daemon; `cli` owns the
//! terminal. [`render_to_text`] is the headless path `--dump` and the tests use.

pub mod action;
pub mod app;
pub mod cli;
pub mod client;
pub mod model;
pub mod view;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use action::{read_preflight, set_live_argv, Action, Command, Mode, Preflight};
use model::Tone;

/// Render one frame of `app` at `width`x`height` into plain text, one string
/// per screen row with trailing spaces trimmed.
pub fn render_to_text(app: &app::App, width: u16, height: u16) -> anyhow::Result<Vec<String>> {
    let mut term = Terminal::new(TestBackend::new(width, height))?;
    term.draw(|f| view::draw(f, app))?;
    let buf = term.backend().buffer();
    Ok((0..height)
        .map(|y| {
            let s: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
            s.trim_end().to_string()
        })
        .collect())
}

/// What performing an act came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Done(String, Tone),
    /// The real `set-live-login --yes` must run on the operator's terminal
    /// (it may open a browser sign-in); the caller hands the terminal over.
    NeedsTerminal(Vec<String>),
}

/// Perform one confirmed act. A WebSocket act goes to the daemon over
/// `h`; the set-live carve-out runs its preflight as a child process.
pub async fn perform(h: &client::Handle, a: &Action) -> Outcome {
    // Every note this returns is drawn: the daemon's refusal or a tool's
    // stderr may quote the key (`accounts/<key>/disable`), so each passes the
    // one masker on its way out (V1-CLEAN2).
    match perform_unmasked(h, a).await {
        Outcome::Done(n, t) => Outcome::Done(model::masked_in(&n, a.label()), t),
        other => other,
    }
}

async fn perform_unmasked(h: &client::Handle, a: &Action) -> Outcome {
    match a.command() {
        Command::Ws { method, params } => match h.request(&method, params).await {
            Ok(p) => {
                h.resubscribe().await;
                let verb = match a {
                    Action::Disable(_) => "disabled",
                    Action::Enable(_) => "reactivated",
                    Action::SetShare(..) => "share set for",
                    Action::SetLive(_) => unreachable!("set-live is local"),
                };
                let same = if p["changed"] == false { " (it already was)" } else { "" };
                Outcome::Done(format!("{verb} {}{same}", model::shown_name(a.label())), Tone::Good)
            }
            Err(e) => Outcome::Done(format!("{method} refused: {e}"), Tone::Bad),
        },
        Command::Local { argv } => {
            // ONE refusal path (MERGE-15): env::egress::admit_login_tool,
            // whose first arm is REL-6a5's "not pointed at a stand-in" rule.
            if let Some(why) = action::isolated_set_live_refusal(std::path::Path::new(&argv[0])) {
                return Outcome::Done(why, Tone::Bad);
            }
            if let Some(why) = action::config_dir_refusal() {
                return Outcome::Done(why, Tone::Bad);
            }
            let label = a.label().to_string();
            let pre = match run_captured(&argv).await {
                Ok(o) => o,
                Err(e) => return Outcome::Done(e, Tone::Bad),
            };
            match read_preflight(pre.0, &pre.1, &pre.2) {
                Preflight::Refuse(why) => Outcome::Done(format!("set-live refused: {why}"), Tone::Bad),
                Preflight::AlreadyLive => {
                    match run_captured(&set_live_argv(&label, Mode::RecordAlreadyLive)).await {
                        Ok((true, out, _)) if out.contains(action::ALREADY_LIVE_MARKER) => Outcome::Done(
                            format!("{} already is this machine's login — name recorded", model::shown_name(&label)),
                            Tone::Good,
                        ),
                        Ok(_) => Outcome::Done(
                            "the machine login moved while recording its name — nothing changed; press s again"
                                .into(),
                            Tone::Warn,
                        ),
                        Err(e) => Outcome::Done(e, Tone::Bad),
                    }
                }
                Preflight::Proceed => Outcome::NeedsTerminal(set_live_argv(&label, Mode::Switch)),
            }
        }
    }
}

/// Run a child with stdin closed; `(ok, stdout, stderr)`.
async fn run_captured(argv: &[String]) -> Result<(bool, String, String), String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| format!("{} timed out after 60s", argv[0]))?
    .map_err(|e| format!("could not run {}: {e}", argv[0]))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}
