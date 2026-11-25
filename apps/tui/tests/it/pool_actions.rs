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

/// **V1-ACCT, V1-CLEAN2-I N1**: `perform`'s own route through the masker. The
/// fixture refuses an act it does not serve with `unknown method:
/// accounts/<key>/pool_share_set` — the daemon quoting the raw key back. The
/// note `perform` returns carries the refusal (sentinel: its method's verb)
/// with the key drawn by the one masker, never whole. Driven through the real
/// client against the fixture on an ephemeral port; the address is a
/// made-up `example.com` one.
#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_quoting_the_key_comes_back_masked_through_perform() {
    // #35: the account's own key is its name and is kept in
    // full in the note; the masker is for OTHER addresses in prose.
    const ADDR: &str = "quill@example.com";
    let fx = pool_fixture::serve().await;
    let (tx, _rx) = mpsc::channel(256);
    let h = client::spawn(fx.url.clone(), tx);
    let a = Action::SetShare(ADDR.into(), Some(0.5));
    let out = tokio::time::timeout(Duration::from_secs(5), perform(&h, &a)).await.expect("perform answers");
    let Outcome::Done(note, _) = out else { panic!("a share act is a daemon request: {out:?}") };
    let sent: Vec<String> = fx.requests.lock().unwrap().iter().map(|(m, _)| m.clone()).collect();
    assert!(sent.contains(&format!("accounts/{ADDR}/pool_share_set")), "sentinel: the raw key went to the daemon: {sent:?}");
    assert!(note.contains("refused") && note.contains("pool_share_set"), "sentinel: the note is the daemon's refusal: {note}");
    assert!(note.contains(&format!("accounts/{ADDR}/pool_share_set")), "the key is drawn whole, as the account's name: {note}");
    assert!(!note.contains(&tui::pool::model::shown_name(ADDR)), "and not masked: {note}");
}

/// Whether the `claude-accounts` helper may run at all, whatever named it:
/// only an absolute path to a regular file you own, not writable by group or
/// others, in directories owned by you or root that nobody else can write.
/// A release-bundle build has no default helper, and says so.
#[test]
fn the_accounts_helper_runs_only_from_a_path_nobody_else_can_write() {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use tui::pool::action::{helper_refusal, NO_HELPER_IN_THIS_BUILD};
    let stub = |p: &Path, mode: u32| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();

    let ok = root.join("mine/claude-accounts");
    stub(&ok, 0o755);
    assert_eq!(helper_refusal(&ok), None, "a stub of yours in a private directory runs");

    // No helper in this build (a release bundle's default): refused with the sentence that says why.
    assert_eq!(helper_refusal(Path::new("")).as_deref(), Some(NO_HELPER_IN_THIS_BUILD));
    assert!(NO_HELPER_IN_THIS_BUILD.contains("TUI_POOL_ACCOUNTS_TOOL"));

    let rel = helper_refusal(Path::new("services/threads/scripts/claude-accounts")).unwrap();
    assert!(rel.contains("not an absolute path") && rel.contains("Nothing ran"), "{rel}");

    let gw = root.join("gw/claude-accounts");
    stub(&gw, 0o775);
    assert!(helper_refusal(&gw).unwrap().contains("writable by group or others"));

    let open = root.join("open/apps/tui/claude-accounts");
    stub(&open, 0o755);
    std::fs::set_permissions(root.join("open"), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(helper_refusal(&open).unwrap().contains("writable by group or others"));
    std::fs::set_permissions(root.join("open"), std::fs::Permissions::from_mode(0o755)).unwrap();

    // THE PLANTED CASE: the shape of a path compiled from a build directory in
    // the shared temporary directory, created by "another user" (here: us).
    let planted_root = PathBuf::from(format!("/private/tmp/tui-planted-helper-{}", std::process::id()));
    let planted = planted_root.join("src/services/threads/scripts/claude-accounts");
    stub(&planted, 0o755);
    let why = helper_refusal(&planted);
    let _ = std::fs::remove_dir_all(&planted_root);
    assert!(why.as_deref().is_some_and(|w| w.contains("/private/tmp is writable by group or others")), "{why:?}");
}

/// A checkout build's default helper is the checkout's own script, and that
/// default passes the same check (this tree is yours, in your directories).
#[test]
fn a_checkout_builds_default_helper_is_its_own_script() {
    let d = tui::pool::action::default_accounts_tool().expect("a checkout build has a default helper");
    assert!(d.ends_with("services/threads/scripts/claude-accounts"), "{}", d.display());
}

/// An installed `tui` finds the helper installed beside it (issue #30):
/// `<root>/share/cauldron/scripts/threads/claude-accounts` for a binary in `<root>/bin/`,
/// also through a link to the binary elsewhere, and that copy passes the
/// same check; with no copy there, none.
#[test]
fn an_installed_tui_finds_the_helper_installed_beside_it() {
    use std::os::unix::fs::PermissionsExt;
    use tui::pool::action::{helper_refusal, installed_tool_for, INSTALLED_HELPER};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("root");
    let exe = root.join("bin/board");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "").unwrap();
    assert_eq!(installed_tool_for(&exe), None, "nothing installed yet");
    let helper = root.join(INSTALLED_HELPER);
    std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
    std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(installed_tool_for(&exe).as_deref(), Some(helper.as_path()));
    assert_eq!(helper_refusal(&helper), None, "the installed copy runs");
    let link = dir.path().canonicalize().unwrap().join("elsewhere/bin/board");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&exe, &link).unwrap();
    assert_eq!(installed_tool_for(&link).as_deref(), Some(helper.as_path()), "a link resolves to the root's copy");
}

