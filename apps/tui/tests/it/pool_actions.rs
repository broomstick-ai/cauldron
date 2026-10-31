//! R7.S2 (DEC-37): every act sends exactly the method the daemon serves for
//! it, and nothing else. Driven through the board's real key handling,
//! `perform` and WebSocket client, against a fixture chassis on an ephemeral
//! port that records what it was sent.

#[path = "pool_fixture/mod.rs"]

mod pool_fixture;

use std::time::Duration;

use chrono::Utc;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use tokio::sync::mpsc;
use tui::pool::action::{Action, Command};
use tui::pool::app::App;
use tui::pool::client::{self, In};
use tui::pool::{perform, render_to_text, Outcome};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

/// The closed table: act → method + params.
#[test]
fn each_act_maps_to_its_endpoint() {
    let ws = |m: &str, p| Command::Ws { method: m.into(), params: p };
    assert_eq!(Action::Disable("beta".into()).command(), ws("accounts/beta/disable", json!({})));
    assert_eq!(Action::Enable("beta".into()).command(), ws("accounts/beta/enable", json!({})));
    assert_eq!(
        Action::SetShare("beta".into(), Some(0.25)).command(),
        ws("accounts/beta/pool_share_set", json!({"share": 0.25}))
    );
    assert_eq!(
        Action::SetShare("beta".into(), None).command(),
        ws("accounts/beta/pool_share_set", json!({"share": null}))
    );
    match Action::SetLive("beta".into()).command() {
        Command::Local { argv } => {
            assert!(argv[0].ends_with("claude-accounts"), "{argv:?}");
            assert_eq!(&argv[1..], ["set-live-login", "beta", "--dry-run"]);
        }
        c => panic!("set-live must be the local carve-out, never a daemon write: {c:?}"),
    }
}

async fn fold_until(app: &mut App, rx: &mut mpsc::Receiver<In>, done: impl Fn(&App) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !done(app) {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(In::Frame(f))) => {
                app.on_frame(&f);
            }
            Ok(Some(_)) => {}
            _ => panic!("timed out waiting for the board to fold"),
        }
    }
}

fn disabled(app: &App, label: &str) -> bool {
    app.board().cards.iter().find(|c| c.label == label).unwrap().disabled()
}

#[tokio::test(flavor = "multi_thread")]
async fn d_on_an_enabled_card_disables_it_and_d_again_reactivates() {
    let fx = pool_fixture::serve().await;
    let (tx, mut rx) = mpsc::channel(256);
    let h = client::spawn(fx.url.clone(), tx);
    let mut app = App::new(&fx.url, Utc::now());
    fold_until(&mut app, &mut rx, |a| a.accounts.rows.len() == 4 && a.non_idle.exported).await;

    // Select beta (card 2) and press d, then confirm.
    assert!(app.key(key('2')).is_none());
    assert!(app.key(key('d')).is_none(), "d asks first");
    let a = app.key(key('y')).expect("y confirms");
    assert_eq!(a, Action::Disable("beta".into()));
    let out = perform(&h, &a).await;
    assert!(matches!(&out, Outcome::Done(n, _) if n == "disabled beta"), "{out:?}");
    fold_until(&mut app, &mut rx, |a| disabled(a, "beta")).await;
    let s = render_to_text(&app, 120, 30).unwrap().join("\n");
    assert!(s.contains("2 beta           DISABLED"), "{s}");

    // The same key on a now-disabled card reactivates it.
    app.key(key('d'));
    let a = app.key(key('y')).unwrap();
    assert_eq!(a, Action::Enable("beta".into()));
    perform(&h, &a).await;
    fold_until(&mut app, &mut rx, |a| !disabled(a, "beta")).await;

    // Any other key cancels and sends nothing.
    app.key(key('d'));
    assert!(app.key(key('n')).is_none());

    let sent: Vec<String> = fx.requests.lock().unwrap().iter().map(|(m, _)| m.clone()).collect();
    let writes: Vec<&String> = sent.iter().filter(|m| !m.starts_with("shapes/")).collect();
    assert_eq!(writes, ["accounts/beta/disable", "accounts/beta/enable"], "all sent: {sent:?}");
    for (m, p) in fx.requests.lock().unwrap().iter() {
        if !m.starts_with("shapes/") {
            assert_eq!(p, &json!({}), "{m} sends no note and no share");
        }
    }
    // Reads are subscriptions only — no read request (board-rebuild.md §1).
    assert!(
        sent.iter().filter(|m| m.starts_with("shapes/")).all(|m| m.ends_with("/subscribe")),
        "{sent:?}"
    );
}

/// `--dump` path: connect to the fixture, fold, draw.
#[tokio::test(flavor = "multi_thread")]
async fn dump_draws_the_fixture_pool() {
    let fx = pool_fixture::serve().await;
    let rows = tui::pool::cli::dump(&fx.url, (120, 30), Duration::from_secs(5)).await.unwrap();
    let s = rows.join("\n");
    assert!(s.contains("threads ● live"), "{s}");
    for want in ["1 alpha", "2 beta           WIPED", "3 delta          NO LOGIN", "4 gamma          DISABLED"] {
        assert!(s.contains(want), "missing {want:?}:\n{s}");
    }
}
