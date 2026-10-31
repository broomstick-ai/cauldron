//! **THE LIVE-PUSH MIGRATION — the daemon half.**
//!
//! The account board used to learn almost everything by ASKING: a 300-second
//! timer and an `r` key, both re-reading the whole picture because the daemon
//! had no way to tell it anything had moved. The operator's ruling was that the
//! timer and the key go away:
//!
//! > *"So basically we need to plan a migration of every one of these features
//! > to lean on the threads daemon (not the tui) and disable refresh."*
//!
//! and, on the one symptom that gave the whole thing away:
//!
//! > *"I have to click refresh to get my counts of active running on each
//! > account to update in the UI. that seems to contradict and needs to be
//! > resolved."*
//!
//! Each test here pins one half of the fix, and the shape follows from what the
//! defect actually was. None of them checks that a frame SERIALIZES — the old
//! code's frames serialized perfectly. They check that the frame carries the
//! fact a client would otherwise have had to ask a second question to learn, or
//! that the daemon SPEAKS at a moment it used to pass over in silence.
//!
//! Nothing here runs a real `claude` turn against a real account: the WS tests
//! use the same canned `fake-claude` fixture `ws_protocol.rs` uses, and the
//! rest drive the manager directly.


#[path = "support/bus.rs"]
mod bus;
#[path = "support/fake_login.rs"]
mod fake_login;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use threads::event::Push;
use threads::{build_manager, Config, ThreadManager};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ── fixtures ─────────────────────────────────────────────────────────────

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    // THE CONSUMER LEDGER IS WIRED BY `run_daemon`, so a test that starts one
    // must be pointed away from the operator's real `~/.cauldron` — otherwise
    // every suite run appends to the file he uses to see who is subscribed, and
    // parallel test binaries interleave into it. See `threads::consumers`.
    std::env::set_var(
        "CAULDRON_HOME",
        std::env::temp_dir().join("threads-tests-cauldron"),
    );
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "100");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "300");
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// A daemon config with the gauge's background machinery OFF.
///
/// Deliberate, and the reason is the same one `info_broadcast.rs` gives: these
/// tests are about what the daemon SAYS when a fact moves, and a sweep that
/// shells out to `claude` would both slow them down and inject announcements
/// nobody asked for. `probe_interval_busy_secs` is zeroed for the same reason —
/// the tests that care about the busy cadence set it themselves.
fn config_at(dir: &Path, accounts: &[&str]) -> Config {
    let mut map = std::collections::BTreeMap::new();
    for a in accounts {
        map.insert(
            a.to_string(),
            dir.join(a).to_string_lossy().to_string(),
        );
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts: map,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        ..Default::default()
    }
}

fn manager(dir: &Path, accounts: &[&str]) -> ThreadManager {
    set_env();
    std::fs::create_dir_all(dir.join("accounts")).unwrap();
    for a in accounts {
        std::fs::create_dir_all(dir.join(a)).unwrap();
    }
    build_manager(config_at(dir, accounts)).unwrap()
}

// ── a minimal WS client (same shape as `ws_protocol.rs`'s) ───────────────

struct Client {
    wr: SplitSink<Ws, Message>,
    rd: SplitStream<Ws>,
    pending: VecDeque<Value>,
    next_id: u64,
    /// The push bus, once `subscribe_all` has opened it.
    bus: Option<bus::Bus>,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        let (wr, rd) = ws.split();
        let mut c = Self {
            wr,
            rd,
            pending: VecDeque::new(),
            next_id: 0,
            bus: None,
        };
        let welcome = c.next_frame().await;
        assert_eq!(welcome["type"], "welcome");
        c
    }

    async fn send_raw(&mut self, v: Value) {
        self.wr.send(Message::Text(v.to_string())).await.unwrap();
    }

    async fn next_frame(&mut self) -> Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(20), self.rd.next())
                .await
                .expect("timed out waiting for frame")
                .expect("socket closed")
                .expect("socket error");
            match msg {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => panic!("unexpected close"),
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    async fn request_ok(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = format!("r{}", self.next_id);
        self.send_raw(json!({"type":"request","id":id,"method":method,"params":params}))
            .await;
        loop {
            let f = self.next_frame().await;
            if f["type"] == "response" {
                assert_eq!(f["correlate"], id.as_str());
                assert_eq!(f["outcome"]["type"], "success", "outcome: {f}");
                return f["outcome"]["payload"].clone();
            }
            self.pending.push_back(f);
        }
    }

    /// **THE DAEMON'S PUSHES, FROM ITS OWN BUS** (R2.S4). This sent
    /// `{"type":"subscribe"}` and read the legacy firehose off the socket; that
    /// door is retired. The bus is opened here, where the subscribe was, so the
    /// ordering the tests rely on is unchanged — see `support/bus.rs`.
    async fn subscribe_all(&mut self, m: &threads::ThreadManager) {
        self.bus = Some(bus::Bus::open(m));
    }

    async fn wait_push(&mut self, pred: impl FnMut(&Value) -> bool) -> Value {
        self.bus
            .as_mut()
            .expect("subscribe_all opens the bus before anything waits on it")
            .wait(pred)
            .await
    }
}

async fn start(dir: &Path, accounts: &[&str]) -> (ThreadManager, SocketAddr) {
    let m = manager(dir, accounts);
    let (addr, _h) = threads::chassis::serve(std::sync::Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    // `serve` leaks the handle deliberately: the daemon lives as long as the
    // test, and dropping it here would close the socket the client is about to
    // open.
    std::mem::forget(_h);
    (m, addr)
}

/// A thread PINNED to a named account. `AccountChoice` is externally tagged,
/// so a pin is `{"named": "<label>"}` — pinned rather than auto because these
/// tests assert on the exact label the frame reports, and an auto placement is
/// the daemon's choice to make.
fn thread_settings(cwd: &Path, account: &str) -> Value {
    json!({"cwd": cwd.to_string_lossy(), "model": "sonnet", "account": {"named": account}})
}

// ── 1. THE ACCOUNT ON THE FRAME ──────────────────────────────────────────

// ── `a_liveness_beat_names_the_account_it_is_running_on` IS DELETED (S3) ──
//
// It asserted `f["account"] == "aa-owned"` on a `liveness` beat, and the field
// it pinned is retired: the beat carried a placement for exactly one consumer,
// the account board's `live.py::_note_account`, and that client-side fold is
// deleted. The board reads placement off the `accounts` shape, where a
// thread's row lives under the account it was placed on.
//
// The test below is the one that survived, and it is the one that matters: a
// placement is DECIDED on an edge, so the edge is where it must be named. A 1
// Hz beat repeating it was convenience for a consumer that no longer exists.
// `the_account_key_is_omitted_when_the_placement_is_unknown` also now pins the
// absence of the key on liveness, so the removal cannot silently come back.

/// Status is the EDGE — the lock being taken is exactly the instant a
/// per-account running count changes — so it is the frame that matters most for
/// the count and the one a client would otherwise chase with a `threads.get`.
#[tokio::test]
async fn a_status_edge_names_the_account_the_thread_is_on() {
    let tmp = tempfile::tempdir().unwrap();
    let (_m, addr) = start(tmp.path(), &["aa-owned"]).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;
    let id = c
        .request_ok(
            "threads/create",
            json!({"settings": thread_settings(tmp.path(), "aa-owned")}),
        )
        .await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    let f = c
        .wait_push(|f| f["type"] == "status" && f["thread_id"] == id.as_str())
        .await;
    assert_eq!(
        f["account"], "aa-owned",
        "a status transition must name the account whose running count just moved: {f}"
    );
}

// `the_account_key_is_omitted_when_the_placement_is_unknown` lived here and
// was DELETED at R2.S4. It serialised the legacy `ServerMsg::Status` and
// `ServerMsg::Liveness` frames and asserted their key sets; both frame types
// went with the firehose (`protocol.rs`), so there is no wire form left to pin.
// The placement fact it protected rides the `threads` shape's cells and the
// `accounts` shape's key, and is pinned there (`thread_shapes.rs`,
// `accounts_api_surface.rs`).


/// The daemon-side half of the same fact: `Push` carries it, so a consumer that
/// reads the broadcast directly (the CLI, a test, a future in-process client)
/// gets it without the wire.
#[tokio::test]
async fn the_push_itself_carries_the_account() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned"]);
    let mut rx = m.hub.firehose.subscribe();
    let id = m
        .create_thread(
            None,
            serde_json::from_value(thread_settings(tmp.path(), "aa-owned")).unwrap(),
        )
        .unwrap();
    m.nudge_thread(&id);
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Push::Status { account, .. }) = rx.recv().await {
                return account;
            }
        }
    })
    .await
    .expect("a status push");
    assert_eq!(got.as_deref(), Some("aa-owned"));
}

// ── 2. THE BUSY-RATE SCAN ────────────────────────────────────────────────

/// **A READING AT PLACEMENT TIME, WHICH IS WHERE THE OPERATOR ASKED FOR ONE:**
/// *"It should be happening at least at placement time so should be emitting."*
///
/// Before this, the only poll anywhere near a dispatch was `edge_poll_open`'s
/// pair, and that is gated on the PER-ACCOUNT `edge_poll_wanted` — "is a budget
/// enforcing, or does calibration still want a reading for this account". On a
/// fully-calibrated machine with no enforcing budget, which is the operator's
/// own, that is **false forever**, so no reading was ever taken at placement and
/// the board's usage numbers moved only on the 900s sweep or on a refresh.
///
/// **The distinction this test exists to pin**: `calibrate` is OFF here and
/// nothing enforces, so `edge_poll_wanted` is false and the edge PAIR does not
/// run — and the placement reading is taken anyway, because it sits above that
/// per-account gate. It still obeys the `edge_poll` master switch, which is on
/// here (a dispatch IS a boundary of our own activity, so one switch governs
/// both), and the busy cadence, which supplies its staleness bound.
#[tokio::test]
async fn a_turn_starting_on_a_stale_account_takes_a_reading_and_announces_it() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("accounts")).unwrap();
    std::fs::create_dir_all(tmp.path().join("aa-owned")).unwrap();
    let mut cfg = config_at(tmp.path(), &["aa-owned"]);
    // The two switches the placement reading is under, and NOTHING else:
    // `edge_poll` (a dispatch is a boundary of our own activity) and the busy
    // cadence (which supplies the staleness bound). `calibrate` stays false and
    // the slow sweep stays off, so the per-account `edge_poll_wanted` gate is
    // false and the edge PAIR cannot be what takes this reading.
    cfg.edge_poll = true;
    cfg.probe_interval_busy_secs = 120;
    let m = build_manager(cfg).unwrap();
    let seen = watch(&m);
    let (addr, h) = threads::chassis::serve(std::sync::Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    std::mem::forget(h);
    let mut c = Client::connect(addr).await;
    let id = c
        .request_ok(
            "threads/create",
            json!({"settings": thread_settings(tmp.path(), "aa-owned")}),
        )
        .await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    // This account has never been measured, so its reading is infinitely stale
    // and the dispatch must take one.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    assert!(
        eventually(|| seen
            .lock()
            .unwrap()
            .iter()
            .any(|(r, a)| r == "usage_poll" && a.as_deref() == Some("aa-owned")))
        .await,
        "dispatching onto an unmeasured account must take a reading AND announce \
         it — announced only after `budgets.observe` recorded it, which is the \
         rule `observe_gauge` states: {:?}",
        seen.lock().unwrap()
    );
    // AND THE READING IS REALLY IN THE STORE, not merely announced. The
    // announcement is worthless if the client re-reads and finds nothing, which
    // is exactly why `probe_account` emits inside the `Ok` arm of the write.
    assert!(
        m.budgets.last_observed_at("aa-owned").is_some(),
        "the announcement must follow a recorded observation, never precede one"
    );

    // ── EXACTLY ONE POLL, AND THIS IS A REGRESSION GUARD WITH A SCAR ──────
    //
    // The placement reading and the opening edge PAIR are alternatives, never
    // both: `edge_poll_open` takes one branch or the other and returns. They
    // used to BOTH run, and because the placement reading is fire-and-forget its
    // `claude -p /usage` child overlapped the pair's. Two concurrent `/usage`
    // children against one account is a wasted process in production and, in
    // `selection_prereqs`' scripted-meter tests, a lost update on the position
    // counter those two processes read-modify-write — measured as `41` read
    // three times from a two-`41` sequence, which left the bracket unwitnessed
    // and the window uncalibrated about one parallel run in four.
    //
    // A COUNT is the right guard because it is what the defect actually moved.
    // This config has `calibrate: false` and nothing enforcing, so
    // `edge_poll_wanted` is false, the pair does not run and the closing ladder
    // does not either — one dispatch must therefore produce exactly one reading.
    // Re-introduce the duplicate and this reads 2.
    let polls = m
        .store
        .recent_observations("aa-owned", "session", 10)
        .unwrap()
        .len();
    assert_eq!(
        polls, 1,
        "one dispatch takes ONE reading — the placement reading and the opening \
         edge pair are mutually exclusive, and two overlapping /usage children \
         against one account is the race that made this suite flaky"
    );
}

/// **THE CONSTRAINT THE OPERATOR PUT ON THE BUSY SCAN**, verbatim: *"do not let
/// the busy-rate scan poll accounts that are not involved. Prefer polling the
/// accounts that actually have work on them."*
///
/// An idle account's usage reading does not move, so polling it every two
/// minutes spends a `claude` process to learn nothing — once per idle login,
/// forever. This is the test that says the scan's target set is the work, not
/// the pool.
#[tokio::test]
async fn the_busy_scan_targets_only_the_accounts_with_work_on_them() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned", "bb-owned"]);
    assert_eq!(
        m.busy_poll_targets(),
        Vec::<String>::new(),
        "an idle daemon has nothing for the busy scan to poll, and must not \
         fall back to the whole pool"
    );
    // A turn in flight on ONE of the two accounts, as the DURABLE row a turn
    // leaves behind. That is deliberately the branch under test: it is the one
    // that survives a daemon restart, it is the one a timer-driven scan reads
    // for a turn re-adopted before the thread was hydrated back into memory, and
    // it is the half of `accounts_with_work` that would be easiest to forget.
    let id = m
        .create_thread(
            None,
            serde_json::from_value(thread_settings(tmp.path(), "bb-owned")).unwrap(),
        )
        .unwrap();
    m.store
        .upsert_running_turn(&threads::store::RunningTurnRow {
            thread_id: id.clone(),
            pid: std::process::id() as i64,
            spawn_cookie: "cookie".into(),
            output_path: tmp.path().join("out.jsonl").to_string_lossy().to_string(),
            byte_offset: 0,
            started_at: threads::store::now_rfc3339(),
            prompt: Some("hi".into()),
            message_ids: vec![1],
            attempt: 1,
        })
        .unwrap();
    // A SECOND manager over the same data dir is how that branch is reached
    // honestly: its in-memory thread map is empty, exactly as a daemon's is for
    // the moments between boot and re-adoption, so the durable row is the only
    // evidence the account is busy. Asking the FIRST manager would take the
    // in-memory branch and prove nothing about the durable one.
    let rebooted = manager(tmp.path(), &["aa-owned", "bb-owned"]);
    assert_eq!(
        rebooted.busy_poll_targets(),
        vec!["bb-owned".to_string()],
        "only the login with work on it is a busy-scan target — the idle sibling \
         is not polled at the busy rate"
    );
}

/// The busy scan refuses with a REASON rather than reporting a silent zero, and
/// "nothing is running" is the reason on an idle daemon. A scan that returned
/// `Ok(0)` here would be indistinguishable from one whose probe failed on every
/// account, which is the exact confusion `probe_accounts_once` was repaired for.
#[tokio::test]
async fn an_idle_daemon_says_why_the_busy_scan_did_not_poll() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned"]);
    let err = m
        .probe_busy_accounts()
        .await
        .expect_err("an idle daemon polls nothing");
    assert!(
        err.contains("in flight"),
        "the refusal must name the condition, not merely be an error: {err}"
    );
}

/// **THE CADENCE IS FLOORED, IN ONE PLACE.** The timer and the placement
/// reading's staleness bound are derived from the same accessor precisely so a
/// hand-written `5` in `config.toml` cannot be honoured by one and clamped by
/// the other.
#[test]
fn the_busy_cadence_is_floored_and_switchable() {
    let base = Config::default();
    assert_eq!(
        base.probe_interval_busy_secs, 120,
        "the default busy cadence is 120s — see the field's docs for the four \
         bounds that number satisfies"
    );

    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config_at(tmp.path(), &[]);
    cfg.probe_interval_busy_secs = 5;
    let m = build_manager(cfg).unwrap();
    assert_eq!(
        m.busy_probe_interval(),
        Some(Duration::from_secs(threads::config::PROBE_BUSY_FLOOR_SECS)),
        "a sub-floor interval is CLAMPED, not honoured: below the floor the \
         process cost stops being negligible and the reading has not settled"
    );

    let tmp2 = tempfile::tempdir().unwrap();
    let mut off = config_at(tmp2.path(), &[]);
    off.probe_interval_busy_secs = 0;
    let m2 = build_manager(off).unwrap();
    assert_eq!(
        m2.busy_probe_interval(),
        None,
        "zero switches the busy scan off entirely — and the placement reading \
         with it, because they share the one number"
    );
}

// ── 3. THE POOL IS RELOADABLE ────────────────────────────────────────────

/// **A NEW ACCOUNT USED TO BE INVISIBLE UNTIL THE DAEMON RESTARTED.** The pool
/// was built once in `build_manager` and never replaced, so `accounts.list`,
/// `accounts.status`, `accounts.pressure` and placement all answered from the
/// roster as it stood at boot — no matter how faithfully a client re-read after
/// being told to.
///
/// That is what made the whole push channel a half-measure for the roster: the
/// frame said "re-read this" and the re-read handed back the stale answer.
#[tokio::test]
async fn the_pool_picks_up_an_account_registered_underneath_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned"]);
    assert_eq!(m.account_pool(), vec!["aa-owned".to_string()]);

    // What `scripts/add-account` + `threads accounts adopt` do, from a CHILD
    // process, with the daemon none the wiser: a directory and a registry row.
    std::fs::create_dir_all(tmp.path().join("zz-new")).unwrap();
    let reg = tmp.path().join("accounts.toml");
    let mut toml = std::fs::read_to_string(&reg).unwrap_or_default();
    toml.push_str(&format!(
        "\n[accounts.zz-new]\nconfig_dir = \"{}\"\n",
        tmp.path().join("zz-new").display()
    ));
    std::fs::write(&reg, toml).unwrap();

    assert!(
        m.reload_pool(),
        "the roster moved on disk, so the reload must report a change"
    );
    assert_eq!(
        m.account_pool(),
        vec!["aa-owned".to_string(), "zz-new".to_string()],
        "the account that was created underneath the daemon is now in the pool"
    );
    assert!(
        !m.reload_pool(),
        "A NO-OP RELOAD MUST REPORT NO CHANGE. `info` means 'what you are \
         showing changed'; announcing on a reload that found the same roster \
         would teach the client to poll again, which is the behaviour being \
         deleted"
    );
}

/// **BUDGETS HOLDS THE POOL TOO**, and a second, privately-snapshotted copy
/// would be a second answer to one question. The cell is shared, so a reload
/// reaches the declared-share resolver as well as placement.
#[tokio::test]
async fn a_reload_reaches_the_budgets_copy_of_the_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned"]);
    let declared = |m: &ThreadManager| -> Value {
        m.budgets.pool_share_rows(&["aa-owned".to_string()])["shares"][0]["declared"].clone()
    };
    assert_eq!(declared(&m), Value::Null, "no pool.json, nothing declared");

    // A hand-written `pool.json` — the operator's own editing path.
    std::fs::write(
        tmp.path().join("pool.json"),
        json!({"entries":[{
            "name":"aa-owned",
            "type":"claude_account",
            "login_dir": tmp.path().join("aa-owned").to_string_lossy(),
            "share": 0.5
        }]})
        .to_string(),
    )
    .unwrap();
    assert!(m.reload_pool());
    assert_eq!(
        declared(&m),
        json!(0.5),
        "the DECLARED share is read through the same cell placement reads \
         membership through — two copies would drift"
    );
}

/// A `pool.json` that cannot be parsed must not take placement out of service.
/// At boot, refusing to start on an unhonourable config is right; here the
/// daemon is already serving on a pool that works, and replacing it with
/// nothing because a hand edit is mid-save is strictly worse than keeping it.
#[tokio::test]
async fn a_broken_pool_file_keeps_the_pool_the_daemon_is_already_serving_on() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned"]);
    std::fs::write(tmp.path().join("pool.json"), "{ this is not json").unwrap();
    assert!(!m.reload_pool(), "a broken blob is not a roster change");
    assert_eq!(
        m.account_pool(),
        vec!["aa-owned".to_string()],
        "the standing pool survives an unparseable edit"
    );
}

/// **THE SEMANTIC COLLISION BETWEEN THIS WAVE AND THE 2026-09-18 PLACEMENT
/// FIX — and it is not visible in either change on its own.**
///
/// `Budgets::ensure_default` reconciles the fleet-wide `aui:default` scope to
/// the pool, widen-only, because an account outside that scope is cut by
/// `admit`'s FIRST filter and is invisible to every thread carrying the key —
/// *"if an account is added to the pool, there's a condition where it can never
/// be used."* That fix called it at boot and nowhere else, and said why in its
/// own docs: *"`self.pool` is built once at construction and is read-only for
/// the process's life, so boot is the only instant at which the pool can have
/// changed."*
///
/// **`PoolCell` retires that premise**, which is the whole point of it. A
/// boot-only reconciliation under a reloadable pool reproduces the operator's
/// defect exactly, with a shorter fuse: the new account joins the roster, gets
/// fingerprinted, gets ranked and gets polled — and is still absent from every
/// admission until the daemon restarts. A restart is the thing this wave exists
/// to stop needing, so "it fixes itself next boot" is not an answer here.
///
/// The test drives the real `reload_pool` and then the real `admit`. The row is
/// checked because it is the mechanism; the ADMISSION is checked because it is
/// the property.
#[tokio::test]
async fn a_pool_reload_re_scopes_the_fleet_wide_budget_to_the_roster_it_found() {
    use threads::budgets::{Admission, AdmissionRequest, Demand, ParkCause, Thresholds};
    use threads::gauge::Axis;

    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned"]);

    // BOOT, as `ThreadManager::recover` performs it.
    m.budgets.ensure_default(&m.account_pool()).unwrap();
    assert_eq!(
        m.budgets
            .get_config("aui:default")
            .unwrap()
            .unwrap()
            .accounts,
        vec!["aa-owned".to_string()],
        "boot scoped the fleet-wide key to the pool it booted with"
    );

    // The readings that make the question decidable: the incumbent is over its
    // own threshold and the newcomer is empty. Nothing subtle is being asked of
    // the ranker — only whether the newcomer is in the candidate field at all.
    let reset = (chrono::Utc::now() + chrono::Duration::hours(40)).to_rfc3339();
    m.budgets
        .observe("aa-owned", Axis::WeeklyAll, 100.0, Some(reset.clone()), "usage_poll")
        .unwrap();
    m.budgets
        .observe("zz-new", Axis::WeeklyAll, 1.0, Some(reset), "usage_poll")
        .unwrap();

    let placeable = |name: &str| {
        threads::pool::PoolEntry::claude_account(
            name,
            &tmp.path().join(name).to_string_lossy(),
        )
        .unwrap()
        .placeable()
    };
    let targets = vec![placeable("aa-owned"), placeable("zz-new")];
    let history = vec!["aa-owned".to_string()];
    // Two asks, because that is what the live path does. The first is the
    // ordinary admission; when it parks for continuity, `failover_target` asks
    // AGAIN with history cleared and routes the move through the porting path.
    // The fix under test is upstream of both: whether `zz-new` is in the
    // candidate field at all.
    let ask = |accounts: &[threads::accounts::Placeable], history: &[String]| {
        m.budgets
            .admit(&AdmissionRequest {
                budget_key: Some("aui:default"),
                thresholds: &Thresholds {
                    session: None,
                    fable: None,
                    weekly: Some(95.0),
                    cash: None,
                },
                accounts,
                current_account: "aa-owned",
                pinned: false,
                history_accounts: history,
                unavailable: &[],
                unhealthy: &[],
                demand: Demand {
                    tokens_per_turn: 20_000_000.0,
                    subagents: 12.0,
                },
            })
            .unwrap()
    };

    // **THE DEFECT, BEFORE THE ROSTER MOVES.** The caller already supplies
    // `zz-new` as a placeable target here — deliberately, so that the SCOPE is
    // the only thing that can be hiding it. The thread cannot stay on a 100%
    // account and has nowhere to go.
    let Admission::Park { reason, .. } = ask(&targets, &history) else {
        panic!("the thread cannot stay on a 100% account against a 95% threshold");
    };
    assert!(
        reason.contains("is NOT in budget aui:default's scope"),
        "`zz-new` is cut by `scoped_accounts`, which is the FIRST filter — above \
         thresholds, targets, history and the ranker. The starved-by-scope \
         diagnostic happens to fire in this shape and says so in the operator's \
         own park text, which is the clearest possible statement of the bug: \
         {reason}"
    );
    assert!(
        !matches!(ask(&targets, &[]), Admission::Admit { .. }),
        "and failover cannot rescue it: the second ask re-applies the scope \
         verbatim, which is what `failover_target`'s own contract says about \
         scope — it lists it among the causes no account change can clear"
    );

    // What `scripts/add-account` + `threads accounts adopt` do from a CHILD
    // process, with the daemon none the wiser.
    std::fs::create_dir_all(tmp.path().join("zz-new")).unwrap();
    let reg = tmp.path().join("accounts.toml");
    let mut toml = std::fs::read_to_string(&reg).unwrap_or_default();
    toml.push_str(&format!(
        "\n[accounts.zz-new]\nconfig_dir = \"{}\"\n",
        tmp.path().join("zz-new").display()
    ));
    std::fs::write(&reg, toml).unwrap();
    assert!(m.reload_pool(), "the roster moved on disk");

    // THE MECHANISM.
    let scope = m
        .budgets
        .get_config("aui:default")
        .unwrap()
        .unwrap()
        .accounts;
    assert!(
        scope.iter().any(|a| a == "zz-new"),
        "THE RELOAD MUST RE-SCOPE. Without this the account is in the pool, in \
         `accounts.list`, on `accounts.pressure` and polled by the busy scan, \
         and still cut by the first filter in every admission: {scope:?}"
    );
    assert!(
        scope.iter().any(|a| a == "aa-owned"),
        "WIDEN-ONLY: a reload never drops a name. That is \
         `purge_account_from_scopes`'s job and it is told WHICH name: {scope:?}"
    );

    // THE PROPERTY, ask one: the newcomer is now IN THE FIELD. The thread still
    // parks for continuity — it holds no conversation on any account it could
    // move to, and a move like that is failover's to make so that the transcript
    // is carried rather than forked (the placement fix left that route alone on
    // purpose). What changed is that `zz-new` is now named as admissible instead
    // of being absent from the operator's park text altogether.
    let Admission::Park { cause, reason, .. } = ask(&targets, &history) else {
        panic!("aa-owned is still at 100%: the thread cannot stay");
    };
    assert_eq!(cause, ParkCause::Continuity, "{reason}");
    assert!(
        reason.contains("admissible now: [Placeable(\"zz-new\")]"),
        "the account is in the candidate field now, and the park says so: {reason}"
    );

    // THE PROPERTY, ask two: `failover_target`, history cleared. THIS is the
    // assertion that fails if the reconciliation is removed from `reload_pool`.
    assert_eq!(
        ask(&targets, &[]),
        Admission::Admit {
            switch_to: Some(placeable("zz-new"))
        },
        "an account added under a RUNNING daemon is reachable by a thread that \
         already exists — which is the operator's report, and it needs BOTH \
         halves: the placement fix put the account in the scope, and this wave \
         is what can move the scope without a restart"
    );
}

/// **AND IT RUNS ON THE CHANGED PATH ONLY.** `reload_pool` is called once per
/// watcher tick; re-reading and rewriting a budget row every tick to confirm a
/// fact that cannot have moved is the polling this whole wave is deleting, one
/// layer down. A reload that finds the same roster changed nothing the scope is
/// derived from, so it must touch nothing.
///
/// Observable because a scope narrowed by hand between two reloads survives a
/// no-op reload. (It would not survive a reload that found a NEW account, and
/// that is deliberate and unchanged: the fleet-wide key is the one key the
/// crate rules is not the operator's to scope.)
#[tokio::test]
async fn a_reload_that_found_nothing_new_does_not_touch_the_budget_row() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned", "bb-owned"]);
    m.budgets.ensure_default(&m.account_pool()).unwrap();

    let mut cfg = m.budgets.get_config("aui:default").unwrap().unwrap();
    cfg.accounts = vec!["aa-owned".to_string()];
    m.budgets.set_config(&cfg).unwrap();

    assert!(!m.reload_pool(), "nothing moved on disk");
    assert_eq!(
        m.budgets
            .get_config("aui:default")
            .unwrap()
            .unwrap()
            .accounts,
        vec!["aa-owned".to_string()],
        "a no-op reload does not re-derive a scope it has no new evidence about"
    );
}

// ── 4. THE SILENCES THAT WERE CLOSED ─────────────────────────────────────

/// Every information update that reaches a firehose subscriber, as
/// `(reason, account)`.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

fn watch(m: &ThreadManager) -> Seen {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    let mut rx = m.hub.firehose.subscribe();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Info { reason, account, .. } = p {
                out.lock().unwrap().push((reason, account));
            }
        }
    });
    seen
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..600 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// **THE SUCCESSFUL LOGIN THAT ANNOUNCED NOTHING.**
///
/// The operator's ask was *"Login authenticates — emit"*. Every announcement on
/// that path lived inside the "we actually lifted a bar" branch, so the ORDINARY
/// case — re-authenticating an account BEFORE it expires, which is the diligent
/// thing to do — returned in silence, and the board went on showing the old
/// credential expiry until he pressed refresh.
#[tokio::test]
async fn a_login_on_an_unbarred_account_is_still_announced() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned"]);
    let seen = watch(&m);
    // R12.5b: a reported login clears and announces only once the re-check's
    // trusted read verifies the slot (crossed/unreadable: daemon_login_guard.rs).
    fake_login::sign_in_verified(&m, "aa-owned", "aa-owned@x.test");
    let reopened = m.note_login_restored("aa-owned");
    assert!(
        reopened.is_empty(),
        "nothing was barred — this is exactly the path that used to be silent"
    );
    assert!(
        eventually(|| seen
            .lock()
            .unwrap()
            .iter()
            .any(|(r, a)| r == "login_refreshed" && a.as_deref() == Some("aa-owned")))
        .await,
        "a credential that moved is a display fact, and it names its account: {:?}",
        seen.lock().unwrap()
    );
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|(r, _)| r == "login_restored"),
        "and it must NOT claim `login_restored`, which means capacity appeared \
         where placement believed there was none — nothing here became placeable"
    );
}

/// **THE BELL NOW NAMES THE ACCOUNT.** `account_picture_changed` always
/// broadcast `account: None`, which is correct for the contract (`on info,
/// re-read`) and useless for the narration the operator values: the difference
/// between *"alice-example-com hit its weekly limit"* and *"something about the
/// accounts changed"*. A client cannot recover the name by diffing either — by
/// the time it re-reads, the new picture is the only one it has.
#[tokio::test]
async fn the_account_operations_name_the_account_they_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, addr) = start(tmp.path(), &["aa-owned"]).await;
    let seen = watch(&m);
    let mut c = Client::connect(addr).await;
    c.request_ok(
        &format!("accounts/{}/pool_share_set", "aa-owned"),
        json!({"share":0.5}),
    )
    .await;
    c.request_ok(&format!("accounts/{}/disable", "aa-owned"), json!({}))
        .await;
    c.request_ok(&format!("accounts/{}/enable", "aa-owned"), json!({}))
        .await;
    for reason in ["pool_share", "account_disabled", "account_enabled"] {
        assert!(
            eventually(|| seen
                .lock()
                .unwrap()
                .iter()
                .any(|(r, a)| r == reason && a.as_deref() == Some("aa-owned")))
            .await,
            "{reason} must name the account it is about: {:?}",
            seen.lock().unwrap()
        );
    }
}

/// **"COMING BACK — EMIT."** Going down is the client's job through the health
/// endpoint; coming back is the daemon's, and it cannot be broadcast at the
/// instant it becomes true because `on_boot` runs before there is a socket to
/// broadcast down.
///
/// **RE-CUT AT R2.S4.** Before: the note was REPLAYED to each legacy firehose
/// subscriber on `{"type":"subscribe"}`, and this test asserted a `lifecycle`
/// frame arrived after that subscribe. That replay is gone with the firehose.
/// Now: the same fact is the `boot` shape's one row, and this test asserts that
/// a consumer subscribing to it OVER THE CHASSIS WIRE is exported this boot's
/// `boot_id`, the `back_up` phase and the `threads` service — the three things
/// the old frame told it. (`boot_shape.rs` proves the shape hub-side; this is
/// the socket half.)
///
/// **Forced red** (R2.S4) by sharing `phase: PHASE_GOING_DOWN` from the `boot`
/// shape: `left: String("going_down")`, `right: "back_up"`.
#[tokio::test]
async fn a_subscriber_learns_on_subscribe_that_the_daemon_is_up() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), &["aa-owned"]);
    // `on_boot` is what computes the note; `run_daemon` calls it before serving
    // and a test that skips it is testing a daemon that never booted.
    m.update.on_boot().await;
    let (addr, h) = threads::chassis::serve(std::sync::Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    std::mem::forget(h);
    let mut c = Client::connect(addr).await;
    c.request_ok(
        "shapes/boot/subscribe",
        json!({"key": "daemon",
               "consumer": {"name": "threads-tests", "shape_version": "1"}}),
    )
    .await;
    let export = loop {
        let f = c.next_frame().await;
        if f["type"] == "shape" && f["op"] == "read" && f["row"].is_object() {
            break f;
        }
    };
    assert_eq!(export["shared"]["phase"], "back_up", "{export}");
    assert_eq!(export["shared"]["service"], "threads", "{export}");
    assert_eq!(
        export["row"]["boot_id"], m.boot_id.as_str(),
        "the row carries THIS boot's id, which is the only thing that lets a \
         client tell 'the daemon restarted under me' from 'my socket blipped': {export}"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
