//! R7.S2 (DEC-37): the pool board against a REAL, THROWAWAY threads daemon —
//! the `threads` crate's own manager and chassis, on `127.0.0.1:0`, with a
//! tempdir data dir and the fake claude binary (no model session is ever
//! started). Proves the board reads the daemon's own shape surface, and that
//! `d` changes the daemon's state, which comes back on the shape.

// REL-6a3 rule B: every child this test binary spawns inherits CAULDRON_ISOLATED=1.
test_isolation::install!();

// V1-POOLTEST: the throwaway daemon's `accounts` shape hydrates login slots,
// which reaches the keychain reader; the threads crate's ONE shared fixture
// (CON-28, R9.F2d) makes every store read as absent, so this test never reads
// his real keychain (without it the CON-28 tripwire aborts the binary).
#[path = "../../../services/threads/tests/it/support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::Utc;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;
use tui::pool::action::Action;
use tui::pool::app::App;
use tui::pool::client::{self, In};
use tui::pool::{perform, render_to_text, Outcome};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

async fn throwaway_daemon() -> (String, tempfile::TempDir) {
    keychain::install();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../services/threads/tests/fixtures/fake-claude"),
    );
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_HOME", tmp.path());
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    threads::clock::testing::ticking();
    let mut accounts = BTreeMap::new();
    for a in ["alpha", "beta"] {
        accounts.insert(a.to_string(), tmp.path().join(a).to_string_lossy().to_string());
    }
    let config = threads::Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };
    let m = threads::build_manager(config).unwrap();
    let (addr, h) = threads::chassis::serve(std::sync::Arc::new(m), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    std::mem::forget(h);
    let url = format!("ws://{addr}/ws");
    // V1-POOLTEST (REL-6a6 egress allowlist): this daemon is a SCRATCH
    // listener this process just bound in-process on 127.0.0.1:0, so the test
    // grants exactly that bound listener — the same as the `tests/it` fixtures.
    // Bound first, then granted from the listener's own `local_addr()` (never
    // from config), which is the shape REL-6a8 keeps (`grant_addr(addr)`).
    // Sentinel: before the grant the gate REFUSES this very port, so the grant
    // — not a hole in the gate — is what admits the dial below.
    let before = tui::cauldron::live::refuse_isolated_url("pool", &url);
    assert!(before.is_some(), "the gate admitted an ungranted scratch port {url} — the test proves nothing");
    env::egress::grant_addr(addr).expect("grant the throwaway daemon's bound port");
    assert_eq!(tui::cauldron::live::refuse_isolated_url("pool", &url), None, "the granted scratch daemon is refused");
    (url, tmp)
}

/// V1-POOLTEST: a grant widens the gate by THAT bound port only. A loopback port this process never bound (an ephemeral listener
/// bound here and NOT granted) is still refused by the pool client's gate, and
/// `client::spawn` panics on it rather than dialling.
#[tokio::test(flavor = "multi_thread")]
async fn the_gate_still_refuses_a_port_this_process_did_not_grant() {
    // Its own listeners, not a second throwaway daemon: `throwaway_daemon`
    // sets process-wide env, and the two tests here run in parallel.
    let mine = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    env::egress::grant_addr(mine.local_addr().unwrap()).expect("grant");
    let granted_url = format!("ws://{}/ws", mine.local_addr().unwrap());
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = other.local_addr().unwrap().port();
    let url = format!("ws://127.0.0.1:{port}/ws");
    assert_ne!(Some(port), client::url_port(&granted_url), "sentinel: two different ports");
    assert_eq!(tui::cauldron::live::refuse_isolated_url("pool", &granted_url), None, "the granted one is admitted");
    let why = tui::cauldron::live::refuse_isolated_url("pool", &url).expect("an ungranted port was admitted");
    assert!(why.contains("was not given"), "{why}");
    let (tx, _rx) = mpsc::channel(8);
    let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| client::spawn(url.clone(), tx)));
    assert!(spawned.is_err(), "client::spawn dialled an ungranted port {url}");
}

async fn fold_until(app: &mut App, rx: &mut mpsc::Receiver<In>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !done(app) {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(In::Frame(f))) => {
                app.on_frame(&f);
            }
            Ok(Some(In::Offline(why))) => panic!("offline while waiting for {what}: {why}"),
            Ok(Some(e)) => tui::pool::cli::apply(app, e),
            _ => panic!(
                "timed out waiting for {what}; board:\n{}",
                render_to_text(app, 120, 30).unwrap().join("\n")
            ),
        }
    }
}

fn is_disabled(app: &App, l: &str) -> Option<bool> {
    app.board().cards.iter().find(|c| c.label == l).map(|c| c.disabled())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_board_draws_and_drives_a_throwaway_daemon() {
    let (url, _tmp) = throwaway_daemon().await;
    let (tx, mut rx) = mpsc::channel(1024);
    let h = client::spawn(url.clone(), tx);
    let mut app = App::new(&url, Utc::now());
    fold_until(&mut app, &mut rx, "both accounts", |a| {
        is_disabled(a, "alpha").is_some() && is_disabled(a, "beta").is_some()
    })
    .await;
    fold_until(&mut app, &mut rx, "the fleet roster", |a| a.non_idle.exported).await;
    app.now = Utc::now();
    let before = render_to_text(&app, 120, 30).unwrap();
    println!("BEFORE:\n{}", before.join("\n"));
    assert!(before.iter().any(|l| l.contains("1 alpha")), "{before:#?}");
    assert!(before.iter().any(|l| l.contains("2 beta")), "{before:#?}");
    assert!(before.iter().all(|l| !l.contains("DISABLED")), "{before:#?}");
    assert!(before[0].contains("▶ 0 running"), "roster from threads/non-idle: {}", before[0]);
    assert!(before[0].contains("threads ● live"), "{}", before[0]);

    // d on beta → the daemon disables it → the accounts shape says so.
    app.key(key('2'));
    app.key(key('d'));
    let a = app.key(key('y')).unwrap();
    assert_eq!(a, Action::Disable("beta".into()));
    let out = perform(&h, &a).await;
    assert!(matches!(&out, Outcome::Done(n, _) if n.starts_with("disabled beta")), "{out:?}");
    fold_until(&mut app, &mut rx, "beta disabled on the shape", |a| is_disabled(a, "beta") == Some(true)).await;
    app.now = Utc::now();
    let after = render_to_text(&app, 120, 30).unwrap();
    println!("AFTER:\n{}", after.join("\n"));
    assert!(after.iter().any(|l| l.contains("2 beta") && l.contains("DISABLED")), "{after:#?}");
    assert_eq!(is_disabled(&app, "alpha"), Some(false));

    // And back.
    app.key(key('d'));
    let a = app.key(key('y')).unwrap();
    assert_eq!(a, Action::Enable("beta".into()));
    perform(&h, &a).await;
    fold_until(&mut app, &mut rx, "beta reactivated", |a| is_disabled(a, "beta") == Some(false)).await;
}
