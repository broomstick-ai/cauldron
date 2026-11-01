//! `tui pool [--dump] [--size WxH] [--seconds N]` — the one place the pool
//! board owns a terminal.
//!
//! The daemon is the one the chat face would reach: `seam::link::ws_url()`,
//! which reads `TUI_THREADS_CHASSIS_PORT` — set by `main` from the cauldron
//! environment before this runs, exactly as for the chat face.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use chrono::Utc;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc;

use super::app::{App, LinkState};
use super::client::{self, In};
use super::model::Tone;
use super::{perform, render_to_text, view, Outcome};

const USAGE: &str = "usage: tui pool [--dump] [--size WxH] [--seconds N]
  the threads account-pool board (replaces scripts/threads-app)
  --dump       connect, wait for the first exports, print one frame, exit
  --size       frame size for --dump (default 120x40)
  --seconds    how long --dump waits for the exports (default 3)";

struct Args {
    dump: bool,
    size: (u16, u16),
    seconds: f64,
}

fn parse(argv: Vec<String>) -> anyhow::Result<Args> {
    let mut a = Args { dump: false, size: (120, 40), seconds: 3.0 };
    let mut it = argv.into_iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--dump" => a.dump = true,
            "--size" => {
                let v = it.next().context("--size needs WxH")?;
                let (w, h) = v.split_once('x').context("--size is WxH")?;
                a.size = (w.parse()?, h.parse()?);
            }
            "--seconds" => a.seconds = it.next().context("--seconds needs N")?.parse()?,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("tui pool: unknown argument `{other}`\n{USAGE}"),
        }
    }
    Ok(a)
}

pub fn main(argv: Vec<String>) -> anyhow::Result<()> {
    let args = parse(argv)?;
    // REL-6a3 rule A: a dump is non-interactive (main has already required
    // its explicit port; this marks an in-process caller too), so the board's
    // connection refuses his ports and its children inherit the marker.
    if args.dump {
        crate::cauldron::live::mark_non_interactive("tui pool --dump");
    }
    let url = crate::seam::link::ws_url();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        if args.dump {
            for row in dump(&url, args.size, Duration::from_secs_f64(args.seconds)).await? {
                println!("{row}");
            }
            Ok(())
        } else {
            interactive(&url).await
        }
    })
}

/// Send the keyed subscribes the roster now calls for.
pub async fn hold_wanted(app: &mut App, h: &client::Handle) {
    for (shape, key) in app.take_wanted() {
        h.hold(shape, &key).await;
    }
}

/// Fold one event from the connection into the board.
pub fn apply(app: &mut App, e: In) {
    match e {
        In::Live => {
            app.link = LinkState::Live;
            // A new session holds no keys; ask for them again.
            app.forget_requested();
        }
        In::Offline(why) => app.link = LinkState::Offline(why),
        In::Frame(f) => {
            app.on_frame(&f);
        }
        In::Refused(why) => app.set_note(why, Tone::Bad),
    }
}

/// Connect, fold until both shapes have exported (or `wait` passes), render.
pub async fn dump(url: &str, size: (u16, u16), wait: Duration) -> anyhow::Result<Vec<String>> {
    let (tx, mut rx) = mpsc::channel(256);
    let h = client::spawn(url.to_string(), tx);
    let mut app = App::new(url, Utc::now());
    let deadline = tokio::time::Instant::now() + wait;
    while !(app.accounts.exported && app.non_idle.exported) {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(e)) => apply(&mut app, e),
            _ => break,
        }
    }
    // A short settle so frames that arrive with the export are folded too —
    // including the keyed names and reasons the roster asked for.
    hold_wanted(&mut app, &h).await;
    while let Ok(Some(e)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
        apply(&mut app, e);
        hold_wanted(&mut app, &h).await;
    }
    app.now = Utc::now();
    render_to_text(&app, size.0, size.1)
}

async fn interactive(url: &str) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::channel(256);
    let h = client::spawn(url.to_string(), tx);
    let mut app = App::new(url, Utc::now());

    // Keys come from a thread, because crossterm's reader blocks. `paused`
    // stops it reading while the terminal is handed to a child.
    let paused = Arc::new(AtomicBool::new(false));
    let (ktx, mut krx) = mpsc::channel(64);
    {
        let paused = paused.clone();
        std::thread::spawn(move || loop {
            if paused.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            if let Ok(true) = event::poll(Duration::from_millis(100)) {
                if let Ok(ev) = event::read() {
                    if ktx.blocking_send(ev).is_err() {
                        return;
                    }
                }
            }
        });
    }

    term_shell::install_panic_hook();
    let claims = [term_shell::ALT_SCREEN, term_shell::HIDE_CURSOR];
    let mut term = term_shell::enter_with(&claims)?;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let result: anyhow::Result<()> = async {
        loop {
            app.now = Utc::now();
            term.draw(|f| view::draw(f, &app))?;
            if app.quit {
                return Ok(());
            }
            tokio::select! {
                e = rx.recv() => if let Some(e) = e {
                    apply(&mut app, e);
                    hold_wanted(&mut app, &h).await;
                },
                k = krx.recv() => {
                    let Some(Event::Key(k)) = k else { continue };
                    if k.kind != KeyEventKind::Press { continue }
                    let Some(a) = app.key(k) else { continue };
                    app.set_note(format!("sending — {}", a.question_as(&app.shown(a.label()))), Tone::Muted);
                    term.draw(|f| view::draw(f, &app))?;
                    match perform(&h, &a).await {
                        // `perform` names the account by the one masker; a twin
                        // is told apart here, where the board is (V1-ACCT).
                        Outcome::Done(n, t) => app.set_note(n.replace(&crate::pool::model::shown_name(a.label()), &app.shown(a.label())), t),
                        Outcome::NeedsTerminal(argv) if crate::pool::action::isolated_set_live_refusal(std::path::Path::new(&argv[0])).is_some() => {
                            let why = crate::pool::action::isolated_set_live_refusal(std::path::Path::new(&argv[0])).unwrap_or_default();
                            app.set_note(why, Tone::Bad);
                        }
                        Outcome::NeedsTerminal(argv) => {
                            paused.store(true, Ordering::SeqCst);
                            term_shell::restore();
                            let st = std::process::Command::new(&argv[0]).args(&argv[1..]).status();
                            term = term_shell::enter_with(&claims)?;
                            term.clear()?;
                            paused.store(false, Ordering::SeqCst);
                            match st {
                                Ok(s) if s.success() => app.set_note(format!("{} is now this machine's login", app.shown(a.label())), Tone::Good),
                                Ok(s) => app.set_note(format!("set-live-login exited {s}"), Tone::Bad),
                                Err(e) => app.set_note(format!("could not run set-live-login: {e}"), Tone::Bad),
                            }
                            h.resubscribe().await;
                        }
                    }
                }
                _ = tick.tick() => {}
            }
        }
    }
    .await;
    term_shell::restore();
    result
}
