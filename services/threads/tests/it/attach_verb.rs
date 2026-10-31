//! **SH-A2 — `threads/{thread_id}/attach`** (`docs/SHIMS.md` §A.1, §A.4).
//!
//! What is proved here, each test naming its forced red (SH-A2 report,
//! an internal report in substrate-interface-layer):
//!
//! * `attach_on_a_projected_thread_answers_a_recipe_without_print_flags_and_records_the_row`
//!   — over the real `/ws` door: a row, the lock held, a recipe with NO print
//!   flag and with `--settings`, both files `0600`, the ledger's turn opened
//!   with no prompt. The print argv of the SAME config is asserted to carry the
//!   flags first, so the absence is not vacuous. Forced red by building the
//!   recipe from `build_args` (the print argv).
//! * `attach_on_a_native_owned_thread_is_refused_and_leaves_nothing` — the
//!   opposing case: same verb, same daemon, a thread left `native_owned`, is
//!   refused `unsupported` and leaves no row, no lock, no file, no ledger.
//!   Forced red by deleting the `NotProjected` refusal.
//! * `attach_while_the_turn_lock_is_held_is_refused_never_two_writers` — a held
//!   `TurnLock` refuses the attach (`busy`), a held attach refuses a second
//!   attach AND a daemon turn's lock, and releasing the lock lets the attach
//!   through (the opposing arm). Forced red by not taking the lock.
//! * `a_failure_after_the_row_insert_leaves_no_orphan_row` — a fault injected
//!   right after the row is written: refused, no row, the lock released, both
//!   files gone, the ledger's turn closed; the next attach succeeds (opposing).
//!   Forced red by removing the row delete on that path.
//! * `attach_on_a_missing_thread_is_not_found_and_a_held_thread_is_busy` —
//!   SH-A2c: a thread id that does not exist answers wire code `not_found`,
//!   never `busy`; an existing held thread is `busy` [locked] (opposing).
//!   Forced red by mapping every `StoreError` back to `Locked`, and by
//!   reverting the wire code to `unknown` (an internal report).
//!
//! Nothing here launches claude: `THREADS_CLAUDE_BIN` points at the fake
//! fixture only so a recipe names something that is not a real binary. Every
//! test gets its own tempdir `data_dir`; `THREADS_CLAUDE_HOME` is scratch.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::ledger::turn::ledger_path;
use threads::ledger::{Kind, Ledger};
use threads::runtime::attach::{AttachRefused, AttachRequest};
use threads::settings::{Projection, ThreadSettings};
use threads::spawn::{SpawnConfig, PRINT_ONLY_FLAGS};
use threads::{build_manager, ThreadManager};

const ACCOUNT: &str = "aa-attach";

fn config(tmp: &Path) -> Config {
    // A FIXED scratch dir (the variable is process-wide); every native file
    // under it is keyed by a fresh thread uuid.
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("THREADS_CLAUDE_BIN", "/nonexistent/fake-claude-for-attach");
    std::env::remove_var("THREADS_CHASSIS_PORT");
    let mut accounts = BTreeMap::new();
    accounts.insert(ACCOUNT.to_string(), tmp.join(ACCOUNT).to_string_lossy().to_string());
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    threads::clock::testing::ticking();
    let m = build_manager(config(tmp.path())).unwrap();
    let (addr, h) = threads::chassis::serve(Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    std::mem::forget(h);
    (m, addr, tmp)
}

fn new_thread(m: &ThreadManager, tmp: &Path, projected: bool) -> String {
    let cwd = tmp.join("work").join("x");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    // A print-only flag the settings DO ask for, so its absence from the
    // interactive recipe is a real absence.
    s.max_budget_usd = Some(5.0);
    if projected {
        s.projection = Projection::Projected;
    }
    let id = m.create_thread(None, s).unwrap();
    let stored = ThreadSettings::from_stored(&m.store.get_thread(&id).unwrap().unwrap().settings_json).unwrap();
    assert_eq!(stored.projection.is_projected(), projected, "the fixture's projection stuck");
    id
}

/// Every file under `<data_dir>/turns/<id>/`.
fn turn_files(tmp: &Path, id: &str) -> Vec<PathBuf> {
    match std::fs::read_dir(tmp.join("turns").join(id)) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => vec![],
    }
}

struct Client {
    ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await.unwrap();
        let mut c = Self { ws };
        let _welcome = c.next().await.expect("a welcome");
        c
    }
    async fn next(&mut self) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), self.ws.next()).await {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }
    async fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        use tokio_tungstenite::tungstenite::Message;
        let frame = json!({"type":"request","id":id,"method":method,"params":params});
        self.ws.send(Message::Text(frame.to_string().into())).await.unwrap();
        loop {
            let f = self.next().await.unwrap_or_else(|| panic!("no response to {method}"));
            if f["type"] == "response" && f["correlate"] == id {
                return f;
            }
        }
    }
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_on_a_projected_thread_answers_a_recipe_without_print_flags_and_records_the_row() {
    let (m, addr, tmp) = start().await;
    let id = new_thread(&m, tmp.path(), true);

    // SENTINEL: the print argv of this very thread carries the print flags,
    // including the one its settings asked for — so "absent" below can fail.
    let s = ThreadSettings::from_stored(&m.store.get_thread(&id).unwrap().unwrap().settings_json).unwrap();
    let print = SpawnConfig::from_settings(&id, &s, vec![]).build_args(true);
    for f in ["-p", "--output-format", "--verbose", "--include-partial-messages", "--max-budget-usd"] {
        assert!(print.iter().any(|a| a == f), "sentinel: the print argv carries {f}: {print:?}");
    }

    let mut c = Client::connect(addr).await;
    let r = c
        .request("a1", &format!("threads/{id}/attach"), json!({"wrapper_pid": 4242}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "attach answered: {r}");
    let p = &r["outcome"]["payload"];
    let cookie = p["cookie"].as_str().expect("a cookie").to_string();
    let args: Vec<String> = serde_json::from_value(p["args"].clone()).unwrap();

    // ── no print flag, of any kind ──
    for f in PRINT_ONLY_FLAGS {
        assert!(!args.iter().any(|a| a == f), "the interactive recipe carries print flag {f}: {args:?}");
    }
    // ── the turn's own flags survive ──
    assert_eq!(arg_after(&args, "--model"), Some("fable"), "{args:?}");
    assert_eq!(arg_after(&args, "--session-id"), Some(id.as_str()), "a new thread starts its session: {args:?}");
    assert_eq!(p["resume"], json!(false));
    // ── the hooks file ──
    let settings = PathBuf::from(arg_after(&args, "--settings").expect("--settings"));
    assert!(settings.starts_with(tmp.path().join("turns").join(&id)), "{}", settings.display());
    let hooks: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    for ev in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
        let cmd = hooks["hooks"][ev][0]["hooks"][0]["command"].as_str().unwrap_or_default();
        assert!(cmd.ends_with(&format!("thread-hook {ev}")), "hook {ev}: {hooks}");
    }
    assert!(!hooks.to_string().contains(&cookie), "the cookie rides the env, not the hooks file");
    // ── the door ──
    let mcp = PathBuf::from(arg_after(&args, "--mcp-config").expect("--mcp-config"));
    assert!(mcp.is_file(), "{}", mcp.display());
    #[cfg(unix)]
    {
        assert_eq!(mode(&settings), 0o600, "the hooks file is private");
        assert_eq!(mode(&mcp), 0o600, "the door file is private");
    }
    assert!(!args.iter().any(|a| a.contains("eyJ") || a.contains("Bearer")), "no token on argv: {args:?}");
    // ── the env ──
    let env = &p["env"];
    assert_eq!(env["CLAUDE_CONFIG_DIR"], json!(tmp.path().join(ACCOUNT).to_string_lossy()));
    assert!(env["THREADS_RUN_TOKEN"].as_str().is_some_and(|t| !t.is_empty()), "{env}");
    assert_eq!(env["THREADS_ATTACH_COOKIE"], json!(cookie));
    assert_eq!(env["THREADS_ATTACH_THREAD"], json!(id));
    assert_eq!(env["ANTHROPIC_API_KEY"], Value::Null, "the API key is removed");
    assert_eq!(p["account"], json!(ACCOUNT));

    // ── the row and the lease ──
    let row = m.store.get_attached_session(&cookie).unwrap().expect("the attached row");
    assert_eq!((row.thread_id.as_str(), row.account.as_str(), row.wrapper_pid), (id.as_str(), ACCOUNT, 4242));
    assert!(m.is_locked(&id), "the attach holds the thread");
    assert_eq!(m.attached_cookies(&id), vec![cookie.clone()]);

    // ── the ledger: the turn is open, rendered, and has NO prompt ──
    let l = Ledger::read_jsonl(&ledger_path(tmp.path(), &id)).unwrap();
    let mine: Vec<_> = l.events().iter().filter(|e| e.turn.as_deref() == Some(cookie.as_str())).collect();
    assert_eq!(mine.first().map(|e| e.kind), Some(Kind::TurnStarted), "{mine:?}");
    assert!(mine.iter().any(|e| e.kind == Kind::System && e.body["what"] == "projected"), "{mine:?}");
    assert!(!mine.iter().any(|e| e.kind == Kind::User), "an attach appends no prompt: {mine:?}");
    assert!(!mine.iter().any(|e| e.kind == Kind::TurnEnded), "the turn is open until detach: {mine:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_on_a_native_owned_thread_is_refused_and_leaves_nothing() {
    let (m, addr, tmp) = start().await;
    let id = new_thread(&m, tmp.path(), false);
    let mut c = Client::connect(addr).await;
    let r = c
        .request("n1", &format!("threads/{id}/attach"), json!({"wrapper_pid": 7}))
        .await;
    assert_ne!(r["outcome"]["type"], json!("success"), "a native_owned thread is refused: {r}");
    let text = r.to_string();
    assert!(text.contains("unsupported") && text.contains("not_projected"), "typed refusal: {r}");

    assert!(m.store.list_attached_sessions(&id).unwrap().is_empty(), "no row");
    assert!(!m.is_locked(&id), "the lock was released");
    assert!(m.attached_cookies(&id).is_empty());
    assert!(turn_files(tmp.path(), &id).is_empty(), "no file: {:?}", turn_files(tmp.path(), &id));
    assert!(!ledger_path(tmp.path(), &id).exists(), "no ledger was started for a native_owned thread");
    // The thread is unflipped: refusal is the chosen arm, not a silent flip.
    let s = ThreadSettings::from_stored(&m.store.get_thread(&id).unwrap().unwrap().settings_json).unwrap();
    assert!(!s.projection.is_projected(), "attach did not flip the thread");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_while_the_turn_lock_is_held_is_refused_never_two_writers() {
    let (m, _addr, tmp) = start().await;
    let id = new_thread(&m, tmp.path(), true);
    let req = AttachRequest { wrapper_pid: 1, account: None };

    // A turn holds the thread.
    let turn = m.try_lock_thread(&id, "turn").expect("the turn takes the lock");
    match m.attach_thread(&id, &req) {
        Err(AttachRefused::Locked(_)) => {}
        other => panic!("an attach under a held TurnLock must be refused Locked, got {other:?}"),
    }
    assert!(m.store.list_attached_sessions(&id).unwrap().is_empty(), "no row while refused");
    assert!(turn_files(tmp.path(), &id).is_empty(), "no file while refused");
    assert!(!ledger_path(tmp.path(), &id).exists(), "the refused attach rendered nothing");
    turn.release();
    drop(turn);

    // OPPOSING: the lock free, the same attach goes through.
    let recipe = m.attach_thread(&id, &req).expect("the attach succeeds once the lock is free");
    assert_eq!(m.store.list_attached_sessions(&id).unwrap().len(), 1);

    // And while ATTACHED: no second attach, and no daemon turn.
    match m.attach_thread(&id, &req) {
        Err(AttachRefused::Locked(_)) => {}
        other => panic!("a second attach must be refused Locked, got {other:?}"),
    }
    assert!(m.try_lock_thread(&id, "turn").is_err(), "a daemon turn cannot start under an attach");
    let rows = m.store.list_attached_sessions(&id).unwrap();
    assert_eq!(rows.len(), 1, "exactly one writer: {rows:?}");
    assert_eq!(rows[0].cookie, recipe.cookie);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_after_the_row_insert_leaves_no_orphan_row() {
    let (m, _addr, tmp) = start().await;
    let id = new_thread(&m, tmp.path(), true);
    let req = AttachRequest { wrapper_pid: 9, account: None };

    m.fail_next_attach_after_insert_for_test();
    let err = m.attach_thread(&id, &req).expect_err("the injected fault refuses the attach");
    assert!(err.to_string().contains("injected fault after the attached row"), "{err}");
    assert!(m.store.list_attached_sessions(&id).unwrap().is_empty(), "NO ORPHAN ROW");
    assert!(!m.is_locked(&id), "the lock was released");
    assert!(m.attached_cookies(&id).is_empty());
    assert!(turn_files(tmp.path(), &id).is_empty(), "both files removed: {:?}", turn_files(tmp.path(), &id));
    // The opened projected turn was closed, so the ledger is not left dirty.
    let l = Ledger::read_jsonl(&ledger_path(tmp.path(), &id)).unwrap();
    assert!(l.events().iter().any(|e| e.kind == Kind::TurnEnded), "the failed attach's turn is closed");
    assert!(threads::ledger::turn::dirty_mark(&l).is_none(), "the ledger is clean");

    // OPPOSING: no fault, a row.
    let ok = m.attach_thread(&id, &req).expect("the next attach succeeds");
    assert!(m.store.get_attached_session(&ok.cookie).unwrap().is_some());
    assert!(m.is_locked(&id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_naming_another_account_is_refused() {
    let (m, _addr, tmp) = start().await;
    let id = new_thread(&m, tmp.path(), true);
    let req = AttachRequest { wrapper_pid: 1, account: Some("someone-else".into()) };
    match m.attach_thread(&id, &req) {
        Err(AttachRefused::AccountMismatch { thread, asked }) => {
            assert_eq!((thread.as_str(), asked.as_str()), (ACCOUNT, "someone-else"));
        }
        other => panic!("expected AccountMismatch, got {other:?}"),
    }
    assert!(!m.is_locked(&id));
    // OPPOSING: naming its own account is accepted.
    let own = AttachRequest { wrapper_pid: 1, account: Some(ACCOUNT.into()) };
    m.attach_thread(&id, &own).expect("its own account attaches");
}

/// SH-A2c (DEPLOY-9i finding 2): a thread that does not exist is NOT FOUND —
/// wire code `not_found` over the real `/ws` door — never `busy`
/// ("the thread is held"). OPPOSING, on the same daemon and the same door: a
/// thread that exists and IS held answers `busy` [locked], so neither a
/// constant "not found" nor a constant "busy" passes. Forced red by mapping
/// every `StoreError` back to `Locked` (the pre-SH-A2c answer).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_on_a_missing_thread_is_not_found_and_a_held_thread_is_busy() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    let missing = "sh-a2c-no-such-thread";
    assert!(m.store.get_thread(missing).unwrap().is_none(), "the id really does not exist");
    let r = c.request("m1", &format!("threads/{missing}/attach"), json!({"wrapper_pid": 7})).await;
    assert_ne!(r["outcome"]["type"], json!("success"), "{r}");
    // The CODE a client branches on (DEPLOY-9k finding 2), not the prose.
    assert_eq!(r["outcome"]["code"], json!("not_found"), "typed not-found: {r}");
    let text = r.to_string();
    assert!(!text.contains("busy") && !text.contains("held"), "a missing thread is never busy/held: {r}");
    match m.attach_thread(missing, &AttachRequest { wrapper_pid: 7, account: None }) {
        Err(AttachRefused::NotFound { thread }) => assert_eq!(thread, missing),
        other => panic!("a missing thread must be NotFound, got {other:?}"),
    }
    assert!(m.store.list_attached_sessions(missing).unwrap().is_empty(), "no row for a missing thread");

    // OPPOSING: an existing thread under a held TurnLock is busy, not not-found.
    let id = new_thread(&m, tmp.path(), true);
    let turn = m.try_lock_thread(&id, "turn").expect("the turn takes the lock");
    let r = c.request("h1", &format!("threads/{id}/attach"), json!({"wrapper_pid": 7})).await;
    assert_ne!(r["outcome"]["type"], json!("success"), "{r}");
    assert_eq!(r["outcome"]["code"], json!("busy"), "a held thread is busy: {r}");
    let text = r.to_string();
    assert!(text.contains("locked"), "a held thread is busy [locked]: {r}");
    assert!(!text.contains("not_found"), "a held thread is never not-found: {r}");
    turn.release();
    drop(turn);
}
