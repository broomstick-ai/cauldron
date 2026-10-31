//! **ONE DERIVATION.** Two people asking whether a thread is running must not
//! be able to get two different answers.
//!
//! > *"Any time we have a user that is requesting information and it requires
//! >  querying the system, then we need the underlying system to do the query.
//! >  Because right now, when we are asking if a thread is idle, it's not going
//! >  to respond correctly, even though it will on its own be able to know if a
//! >  thread is idle. That's a mistake. **We shouldn't have the ability to get
//! >  two different answers when two different people ask.**"*
//!
//! WHAT WENT WRONG. Two arena threads reported `busy` for twenty-five hours
//! after completing successfully. Nine independent places derived (or echoed) a
//! thread's activity; `threads.list` and `threads.get` echoed a cached
//! `threads.status` COLUMN, written by three separate paths and repaired by
//! four more, and the terminal path wrote `busy` on a turn that had already
//! ended (the status was computed while the `TurnLock` was still held). The
//! column is deleted. `ThreadManager::thread_states` is the only derivation
//! left, and every surface renders it.
//!
//! These tests hold that line from the OUTSIDE, over a real WebSocket, because
//! "two different people ask" means two different RPCs. Every scenario asks
//! both `threads.status` and `threads.list` about the same thread at the same
//! moment and requires the same answer. (Since R2.S3c — R4.S9, HOLDS R9 — the
//! `threads.status` REQUEST is retired, so its asker is the derivation it
//! answered with, `ThreadManager::thread_status`; `threads.list` is still asked
//! over the socket.)
//!
//!   * turn running                          * gate-parked (admission refused)
//!   * turn completed                        * retry armed
//!   * completed with a subagent outstanding * daemon died mid-turn
//!   * turn killed and parked                * thread never run
//!
//! Plus the three shapes the column got WRONG and the derivation cannot:
//! a completed turn reported busy, a PANICKED turn reported busy, and a leaked
//! lock row reported active. The query COUNT lives in `derivation_cost.rs`
//! (its own binary, so nothing runs beside it).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use futures::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use threads::store::{now_rfc3339, ParkedTurnRow, RetryRow};
use threads::{build_manager, Config, ThreadManager};

/// **THE UNDELIVERED QUEUE, READ OFF THE `queue` CELL AND FILTERED.**
///
/// This is what `threads/{id}/queue` and `threads/{id}/queue_failed` used to
/// answer, and both methods are gone. They projected exactly the same four
/// fields out of `store::QueueRow` and differed in ONE `WHERE state=`, so the
/// discriminator their NAMES encoded is now a declared field with a closed
/// vocabulary — `shapes::threads::QueueEntry` and `QUEUE_STATE_WORDS` — and a
/// reader FILTERS instead of choosing a method per panel.
///
/// **A FRESH CONNECTION PER READ, deliberately.** The cell arrives as a shape
/// frame, and this file's client sits on the firehose with a `pending` queue
/// that the push assertions walk in order; draining shape frames out of that
/// queue would couple every queue read to whatever push happened to be in
/// flight.
///
/// **A KNOWN-EMPTY QUEUE IS `[]`, NEVER A MISSING KEY.** The cell is `Required`
/// and `shapes::threads`' hydrate maps `Hydration::Empty` onto an empty list,
/// so the panic below fires only if the shape stopped carrying the cell at all
/// — which is the thing worth failing on, and which is why this reads the key
/// rather than defaulting it away.
async fn queue_rows(addr: SocketAddr, id: &str, state: &str) -> Vec<Value> {
    let (ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
    let (mut wr, mut rd) = ws.split();
    let _welcome = rd.next().await;
    wr.send(Message::Text(
        json!({
            "type": "request",
            "id": "queue-cell",
            "method": "shapes/threads/subscribe",
            "params": {
                "key": id,
                // **COMPULSORY SINCE 2026-10-08.** An absent or half-filled
                // identity is REFUSED by this door, and the refusal emits no
                // export — so a helper that omitted it would wait out the full
                // timeout below for what is a missing parameter.
                "consumer": {
                    "name": "threads-tests",
                    "shape_version": "shapes.md@rev4",
                },
            },
        })
        .to_string(),
    ))
    .await
    .unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), rd.next())
            .await
            .expect("timed out waiting for the `threads` export")
            .expect("socket closed before the export")
            .expect("socket error");
        if let Message::Text(t) = msg {
            let f: Value = serde_json::from_str(&t).unwrap();
            // **A REFUSAL IS NAMED RATHER THAN WAITED OUT.** This door answers a
            // subscribe it refuses with an error outcome and then emits nothing,
            // so a reader that watched only for shape frames would report a
            // twenty-second timeout for what is a one-line parameter mistake.
            if f["type"] == "response"
                && f["correlate"] == json!("queue-cell")
                && f["outcome"]["type"] != json!("success")
            {
                panic!("the `threads` subscribe was refused: {}", f["outcome"]);
            }
            if f["type"] == "shape" && f["op"] == "read" && f["key"] == json!(id) {
                return f["row"]["queue"]
                    .as_array()
                    .unwrap_or_else(|| {
                        panic!("the `threads` shape must carry the `queue` cell: {f}")
                    })
                    .iter()
                    .filter(|e| e["state"] == json!(state))
                    .cloned()
                    .collect();
            }
        }
    }
}


type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "100");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "300");
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// Start the daemon on an ephemeral port with one account threads OWNS
/// (CON-27: the machine `default` is never a placement target).
async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        tmp.path().join("aa-owned").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };
    let manager = build_manager(config).unwrap();
    let (addr, _handle) = threads::chassis::serve(std::sync::Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (manager, addr, tmp)
}

// ── a minimal WS client (the ws_protocol.rs shape) ───────────────────────────

struct Client {
    wr: SplitSink<Ws, Message>,
    rd: SplitStream<Ws>,
    pending: VecDeque<Value>,
    next_id: u64,
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
        };
        let _welcome = c.next_frame().await;
        c
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
        self.wr
            .send(Message::Text(
                json!({"type": "request", "id": id, "method": method, "params": params})
                    .to_string(),
            ))
            .await
            .unwrap();
        let outcome = loop {
            let f = self.next_frame().await;
            if f["type"] == "response" {
                assert_eq!(f["correlate"], id.as_str());
                break f["outcome"].clone();
            }
            self.pending.push_back(f);
        };
        assert_eq!(outcome["type"], "success", "outcome: {outcome}");
        outcome["payload"].clone()
    }
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

// ── THE AGREEMENT CHECK ──────────────────────────────────────────────────────

/// One thread's answer, as a `threads.status` row and as a `threads.list` row,
/// asked back to back over the SAME socket — and required to be identical.
///
/// This is the operator's rule made executable. It checks all three wire keys:
///
///   * `state` — the full vocabulary. `threads.status` has always carried it;
///     `threads.list` carries it now because the list used to carry only the
///     cached column, which is what went stale.
///   * `executing` — the boolean AUI's `status_frame_executing` prefers over
///     everything else. It is exactly "a live execution holds the lock".
///   * `status` — the legacy `busy | retrying | idle` projection AUI's
///     `status_str_is_running` reads off a `threads.list` row (accepting ONLY
///     `busy` and `retrying`, pinned by AUI's own tests). Required to be the
///     projection of the SAME derived state, not an independent word.
///
/// Returns the agreed `(state, executing, status_word)`.
async fn agree(c: &mut Client, m: &ThreadManager, id: &str) -> (String, bool, String) {
    // `threads/status` was retired by R2.S3c (R4.S9, HOLDS R9). Its `thread_id`
    // form answered `ThreadManager::thread_status(id)` verbatim, so the second
    // asker is now that call — the claim (two askers, one derivation) is the
    // same, and `threads.list` is still asked over the real socket.
    let st = m.thread_status(id).unwrap();
    let list = c.request_ok("threads/list", json!({ "limit": 1000 })).await;
    let row = list["threads"]
        .as_array()
        .expect("threads.list returns an array")
        .iter()
        .find(|r| r["thread_id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} is missing from threads.list"));

    let state = st["state"].as_str().unwrap().to_string();
    let executing = st["executing"].as_bool().unwrap();

    assert_eq!(
        row["state"], st["state"],
        "threads.list and threads.status disagree about `state` for {id}"
    );
    assert_eq!(
        row["executing"], st["executing"],
        "threads.list and threads.status disagree about `executing` for {id}"
    );

    // WIRE COMPATIBILITY. `status` is the projection of `state`, and nothing
    // else. Breaking this spelling makes AUI read every thread as idle.
    let word = row["status"].as_str().expect("a threads.list row keeps `status`");
    let expected = match state.as_str() {
        "active" => "busy",
        "retrying" => "retrying",
        _ => "idle",
    };
    assert_eq!(
        word, expected,
        "the legacy `status` word on a threads.list row must be the projection \
         of the derived state ({state}) for {id}"
    );
    assert!(
        matches!(word, "busy" | "retrying" | "idle"),
        "AUI's status_str_is_running accepts only busy|retrying (and reads \
         everything else as idle); {word:?} is outside the contract"
    );
    // And the other direction: AUI's `status_frame_executing` accepts only
    // `active` and `retrying` on `state`, so an executing thread must say one
    // of those or the boolean must carry it.
    if executing {
        assert_eq!(
            state, "active",
            "an executing thread must say `active` on `state`"
        );
    }

    (state, executing, word.to_string())
}

fn settings(cwd: &std::path::Path) -> Value {
    json!({"cwd": cwd.to_string_lossy(), "model": "sonnet", "inject_context": false})
}

async fn make_thread(c: &mut Client, cwd: &std::path::Path) -> String {
    c.request_ok("threads/create", json!({"settings": settings(cwd)}))
        .await["thread_id"]
        .as_str()
        .unwrap()
        .to_string()
}

// ── 1. never run → running → completed ───────────────────────────────────────

/// The three states every thread passes through, each read from BOTH surfaces.
///
/// The middle one is the only one the old column ever got right, and the last
/// one is the twenty-five-hour bug: `emit(TurnEnded)` persisted `busy` because
/// it computed the word while the `TurnLock` was still held and `turn_running`
/// was still true. There is nothing left to persist, so there is nothing left
/// to persist wrongly.
#[tokio::test]
async fn list_and_status_agree_before_during_and_after_a_turn() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let id = make_thread(&mut c, tmp.path()).await;

    // NEVER RUN. A thread nobody has sent to is idle, and both surfaces say so.
    assert_eq!(
        agree(&mut c, &m, &id).await,
        ("idle".into(), false, "idle".into()),
        "a thread that has never run is idle"
    );

    // RUNNING. `holdturn` blocks until the test creates the release file, so
    // "the turn is definitely still executing" is a fact, not a sleep.
    let release = tmp.path().join("release-1");
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": format!("holdturn release={}", release.display())}),
    )
    .await;
    assert!(eventually(|| m.is_locked(&id)).await, "the turn never started");
    assert_eq!(
        agree(&mut c, &m, &id).await,
        ("active".into(), true, "busy".into()),
        "a live execution reads as active/busy on both surfaces"
    );

    // COMPLETED.
    std::fs::write(&release, b"go").unwrap();
    assert!(
        eventually(|| !m.is_locked(&id)).await,
        "the turn never finished"
    );
    let (state, executing, word) = agree(&mut c, &m, &id).await;
    assert_eq!(
        (state.as_str(), executing, word.as_str()),
        ("idle", false, "idle"),
        "A COMPLETED TURN IS NEVER BUSY. This is the arena-thread bug: it \
         reported `busy` for twenty-five hours after finishing."
    );
    drop(tmp);
}

// ── 2. a subagent outlives the parent turn ───────────────────────────────────

/// A turn whose async subagent is STILL RUNNING when the parent result lands.
///
/// This is the exact branch that stranded the arena threads. `finish_turn`
/// released the lock, saw `still_busy`, spawned the subagent poller — and took
/// the branch that SKIPPED the compensating status write, leaving whatever
/// `emit(TurnEnded)` had persisted a moment earlier (`busy`) in place forever.
/// The parent turn is over; the thread is not executing; both surfaces must say
/// so while the subagent poller is still running.
#[tokio::test]
async fn a_turn_whose_subagent_outlives_it_is_not_reported_busy() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let id = make_thread(&mut c, tmp.path()).await;

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "subagent: launch one and finish without it"}),
    )
    .await;
    assert!(eventually(|| m.is_locked(&id)).await, "the turn never started");
    // The parent turn ends while the Agent is still outstanding.
    assert!(
        eventually(|| !m.is_locked(&id)).await,
        "the parent turn never released the lock"
    );

    let (state, executing, word) = agree(&mut c, &m, &id).await;
    assert_eq!(
        (state.as_str(), executing, word.as_str()),
        ("idle", false, "idle"),
        "the parent turn has ENDED — an outstanding subagent is liveness, not \
         an execution, and it must not leave the thread reading busy"
    );
    drop(tmp);
}

// ── 3. a killed turn, parked durably ─────────────────────────────────────────

/// A `parked_turns` row — a turn that was torn down with its prompt held.
///
/// Written directly, because the row IS the fact the derivation reads: the
/// budget-kill machinery that produces it is exercised in `budgets_e2e.rs`, and
/// what is under test here is that ONE row produces ONE answer on two surfaces.
#[tokio::test]
async fn list_and_status_agree_on_a_killed_and_parked_turn() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let id = make_thread(&mut c, tmp.path()).await;

    m.store
        .upsert_parked_turn(&ParkedTurnRow {
            thread_id: id.clone(),
            prompt: "held, not dropped".into(),
            message_ids: vec![1],
            attempt: 1,
            reason: "budget:demo session 100%".into(),
            budget_key: Some("demo".into()),
            parked_at: now_rfc3339(),
        })
        .unwrap();

    let (state, executing, word) = agree(&mut c, &m, &id).await;
    assert_eq!((state.as_str(), executing), ("parked", false));
    // The legacy column projected a park to `idle` and so does this — the
    // richer word travels on `state`, which both surfaces now carry.
    assert_eq!(word, "idle");
    let st = m.thread_status(&id).unwrap();
    assert_eq!(st["detail"], json!("budget:demo session 100%"));
    drop(tmp);
}

// ── 4. a GATE park: the shape `thread_status` could not see ──────────────────

/// An ADMISSION-GATE park — the thread's account is over its threshold, so the
/// turn is refused before it starts and the prompt stays QUEUED.
///
/// Only `park_killed_turn` and `park_retried_turn` write `parked_turns`. A gate
/// park lives solely in the in-memory `park_reason` latch, which `park_state`
/// read and `thread_status` did not — so `emit_park` had to call `emit_status`
/// to announce a park whose own frame then said `queued`. `thread_states` reads
/// the same latch, so both shapes of park are one answer now.
#[tokio::test]
async fn a_gate_park_is_visible_to_both_surfaces() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    let mut s = settings(tmp.path());
    s["thresholds"] = json!({"fable": 60.0});
    let id = c.request_ok("threads/create", json!({"settings": s})).await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();

    // The only placeable account is over the thread's fable threshold.
    m.budgets
        .observe(
            "aa-owned",
            threads::gauge::Axis::WeeklyFable,
            88.0,
            None,
            "usage_poll",
        )
        .unwrap();

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "please do the work"}),
    )
    .await;

    assert!(
        eventually(|| m.park_state(&id).unwrap() != Value::Null).await,
        "the admission gate never parked the turn"
    );

    let (state, executing, word) = agree(&mut c, &m, &id).await;
    assert_eq!(
        (state.as_str(), executing, word.as_str()),
        ("parked", false, "idle"),
        "a GATE park must read as parked and not as queued — the prompt is \
         still in the queue, but the thread is waiting on the gate"
    );
    // The prompt really is still queued: `parked` outranks `queued` on purpose.
    let q = queue_rows(addr, &id, "waiting").await;
    assert_eq!(q[0]["message_id"], json!(1));
    drop(tmp);
}

// ── 5. an armed retry ────────────────────────────────────────────────────────

/// A `retries` row is what "retrying" MEANS. The old column carried the word
/// separately and `accounts.status` counted it from there.
#[tokio::test]
async fn list_and_status_agree_on_an_armed_retry() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let id = make_thread(&mut c, tmp.path()).await;

    m.store
        .upsert_retry(&RetryRow {
            thread_id: id.clone(),
            prompt: "try me again".into(),
            message_ids: vec![1],
            attempt: 1,
            kind: "rate_limit".into(),
            reason: "the account said no".into(),
            account: None,
            created_at: String::new(),
        })
        .unwrap();

    let (state, executing, word) = agree(&mut c, &m, &id).await;
    assert_eq!((state.as_str(), executing), ("retrying", false));
    // AUI reads `retrying` as "still working" in the operator's ear, on BOTH
    // the `status` word and the `state` word. Both must survive the wire.
    assert_eq!(word, "retrying");
    drop(tmp);
}

// ── 6. a daemon that died mid-turn ───────────────────────────────────────────

/// A `thread_locks` row left by a daemon that is GONE.
///
/// `pid_alive` is false for it, so it is a reclaimable row and not a live
/// execution — and it must not be reported as one, on either surface. This is
/// also the shape that made every dispatch on the Minecraft thread refuse for
/// hours in August.
#[tokio::test]
async fn a_lock_row_from_a_dead_daemon_does_not_report_active() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let id = make_thread(&mut c, tmp.path()).await;

    // A pid that is definitely not a running process, under a foreign boot id.
    let dead = spawn_and_reap();
    assert!(matches!(
        m.store
            .try_claim_thread(&id, "a-previous-daemon-boot", dead, "dispatch"),
        Ok(threads::store::Claim::Taken)
    ));
    assert!(m.store.get_thread_lock(&id).unwrap().is_some(), "row written");

    let (state, executing, word) = agree(&mut c, &m, &id).await;
    assert_eq!(
        (state.as_str(), executing, word.as_str()),
        ("idle", false, "idle"),
        "a lock row whose holder is dead is reclaimable, not a live execution"
    );
    drop(tmp);
}

/// A pid that has definitely exited AND been reaped, so `kill(pid, 0)` fails
/// and it is not a zombie either.
fn spawn_and_reap() -> i64 {
    let mut child = std::process::Command::new("/usr/bin/true")
        .spawn()
        .expect("spawn /usr/bin/true");
    let pid = child.id() as i64;
    child.wait().expect("reap");
    pid
}

// ── 7. a LEAKED lock row (the inverted rule) ─────────────────────────────────

/// A `thread_locks` row bearing OUR OWN `boot_id` while this process has no
/// turn running on that thread.
///
/// **This is the inverted-rule bug.** `is_locked` called such a row LIVE
/// unconditionally; `lock_turn`'s `Claim::Held` arm called the identical row
/// STALE and stole it. So a `release_thread_lock` that failed left
/// `threads.status` reporting `active` for the rest of the process's life while
/// the dispatcher happily kept running turns on that very thread — the observer
/// and the actor disagreeing about one row, inside one process.
///
/// One predicate now (`lock_is_live`), and `lock_turn`'s reading wins because
/// it is the one the daemon ACTS on: our own boot id with no turn running is a
/// leaked row. The test proves both halves — the status surfaces say idle, AND
/// the dispatcher can still take the lock.
#[tokio::test]
async fn a_leaked_lock_row_does_not_report_active() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let id = make_thread(&mut c, tmp.path()).await;

    // Exactly what a failed release leaves behind: our boot, our pid, no turn.
    let me = std::process::id() as i64;
    assert!(matches!(
        m.store.try_claim_thread(&id, &m.boot_id, me, "dispatch"),
        Ok(threads::store::Claim::Taken)
    ));

    let (state, executing, word) = agree(&mut c, &m, &id).await;
    assert_eq!(
        (state.as_str(), executing, word.as_str()),
        ("idle", false, "idle"),
        "a leaked lock row is a failed release, not an execution"
    );
    assert!(!m.is_locked(&id), "is_locked must read the same row the same way");

    // ...and the dispatcher agrees, which is the half that was never in doubt:
    // it steals the row and runs.
    let lock = m
        .try_lock_thread(&id, "test")
        .expect("the dispatcher reclaims a leaked row");
    assert!(
        m.is_locked(&id),
        "while a real execution holds it, the SAME row is live"
    );
    assert_eq!(agree(&mut c, &m, &id).await.0, "active");
    drop(lock);
    drop(tmp);
}

// ── 8. a PANICKED turn ───────────────────────────────────────────────────────

/// A turn task that panics mid-execution.
///
/// `TurnLock::Drop` released the durable row and broadcast a correct derived
/// push — and wrote NO column, so the persisted word stayed at whatever the
/// last `emit` had left there (`busy`). Only the boot sweep ever repaired it.
/// With the answer derived from the row `Drop` clears, the panic path is
/// correct for free: there is no second thing to keep in step.
#[tokio::test]
async fn a_panicked_turn_is_never_reported_busy() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let id = make_thread(&mut c, tmp.path()).await;

    let m2 = m.clone();
    let id2 = id.clone();
    let handle = tokio::spawn(async move {
        let _lock = m2.try_lock_thread(&id2, "test").expect("lock");
        assert!(m2.is_locked(&id2), "held while the task lives");
        panic!("a turn task dies mid-execution");
    });
    assert!(handle.await.is_err(), "the task must actually have panicked");

    assert!(
        !m.is_locked(&id),
        "Drop is the backstop: a panicking turn must not strand the thread"
    );
    let (state, executing, word) = agree(&mut c, &m, &id).await;
    assert_eq!(
        (state.as_str(), executing, word.as_str()),
        ("idle", false, "idle"),
        "A PANICKED TURN IS NEVER BUSY."
    );
    drop(tmp);
}

// ── 9. accounts.status counts what is actually true ──────────────────────────

/// The `accounts.status` payload. The request was retired by R2.S3c (R4.S9,
/// HOLDS R9); `threads accounts status --json` still renders it, from these
/// same two functions with these same inputs (the retired arm's body,
/// verbatim), so the count is still asked of the one derivation.
fn accounts_status(m: &ThreadManager) -> Value {
    let accounts = m.account_pool();
    let gauge = m.budgets.gauge_state(&accounts).unwrap();
    let rows = threads::accounts::collect_activity(
        &m.store,
        &m.pool(),
        &std::collections::BTreeMap::new(),
        Some(&gauge),
        1000,
    )
    .unwrap();
    threads::accounts::activity_json(&rows)
}

/// `accounts.status`'s `idle` count after a completed turn.
///
/// It used to be computed from the cached column: a thread stuck at a stale
/// `busy` counted as neither `retrying` nor `idle`, so the pool silently looked
/// emptier than it was — the number an agent asks for when it wants to know
/// whether an account is free. `retrying` is derived from the `retries` table
/// now and `idle` follows from it.
#[tokio::test]
async fn accounts_status_idle_count_is_correct_after_a_completed_turn() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let id = make_thread(&mut c, tmp.path()).await;

    let acct = accounts_status(&m)
        .get("accounts")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|r| r["account"] == "aa-owned").cloned())
        .expect("aa-owned reports");
    assert_eq!(acct["idle"], json!(1), "a fresh thread is idle");

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "a normal, successful turn"}),
    )
    .await;
    assert!(eventually(|| m.is_locked(&id)).await, "the turn never started");
    assert!(
        eventually(|| !m.is_locked(&id)).await,
        "the turn never finished"
    );
    // The ledger's `turn_ended` has landed and the lock is gone; nothing else
    // is outstanding.
    assert_eq!(agree(&mut c, &m, &id).await.0, "idle");

    let acct = accounts_status(&m)
        .get("accounts")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|r| r["account"] == "aa-owned").cloned())
        .expect("aa-owned reports");
    assert_eq!(
        acct["idle"],
        json!(1),
        "a thread that finished its turn is IDLE — the stale `busy` used to \
         make it count as neither idle nor retrying"
    );
    assert_eq!(acct["executing"], json!(0));
    assert_eq!(acct["retrying"], json!(0));
    drop(tmp);
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
