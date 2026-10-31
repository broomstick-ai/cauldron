//! Budgets module end-to-end (§15 of the CCW-migration design doc).
//!
//! Every test drives the daemon in-process on an ephemeral port against the
//! FAKE claude fixture — **no real claude turn is ever spawned**, including
//! the `/usage` probe, which the fixture answers with CCW's observed prose
//! shape. Usage is scarce; a test that spawns real claude is a defect.
//!
//! What is asserted here, decision by decision:
//!
//! * DEC-4/DEC-14 — keyed budgets: optional per-thread key, shared keys,
//!   re-pointing, keys outliving threads.
//! * DEC-6/DEC-10 — thresholds gate over the account POOL and PARK the turn.
//! * DEC-12/DEC-15 — the layered gauge feeds a continuously re-estimated
//!   per-account per-axis token limit, and never fabricates one.
//! * DEC-13/DIR-19/CON-4 — a trip with `kill` tears the execution down AND
//!   parks the operator's prompt; nothing is dropped.
//! * DEC-17 — budget state transitions arrive as first-class thread events.
//! * DEC-19 — `usage_cap_fraction` is gone.
//!
//! …and the operator's ROUND-7 corrections, each with its own test at the
//! bottom of the file:
//!
//! * item 1 — an internal failure fails CLOSED, loudly, and `budgeting_off` is
//!   the bypass that lets the operator keep working while it is fixed.
//! * item 3 — a trip lets the execution FINISH by default; kill is a toggle.
//! * item 4/5 — budgets on all three percentage axes plus a dollar cash axis.
//! * item 6 — the crate refuses to default a budget to every account.
//! * item 7 — a thread with history parks rather than losing its conversation.


#[path = "support/bus.rs"]
mod bus;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use futures::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use threads::store::EstimateRow;
use threads::{build_manager, Config};

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
    std::env::set_var("THREADS_KILL_GRACE_MS", "200");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    // The machine-default account's transcript store is `<HOME>/.claude` — the
    // OPERATOR'S OWN login. Point the transcript home at a scratch dir so no
    // test can read or write inside it (account moves now port transcripts).
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// The thread's STORED settings, spelled as `threads/{thread_id}/get` spelled
/// them (`{"settings": <reparsed blob>}`). That request was retired by R2.S3c
/// (R4.S9, HOLDS R9); the round-trip claims below are about what was stored,
/// so they read the store.
fn stored_thread(m: &threads::ThreadManager, id: &str) -> Value {
    let row = m.store.get_thread(id).unwrap().expect("the thread exists");
    json!({ "settings": serde_json::from_str::<Value>(&row.settings_json).unwrap() })
}

async fn start() -> (threads::ThreadManager, SocketAddr, tempfile::TempDir) {
    start_with_teardown(None).await
}

async fn start_with_teardown(
    teardown: Option<&str>,
) -> (threads::ThreadManager, SocketAddr, tempfile::TempDir) {
    start_full(teardown, &[]).await
}

/// A daemon with the gauge's own POLLING switched off — no background sweep,
/// no calibration polling, no opening/closing edge polls.
///
/// The tests that assert on *which reading the gate used* need this: with edge
/// polling on (the default), every turn brackets itself with `/usage` polls,
/// and the fixture's fixed 12/34/56 answer would land on top of the reading the
/// test is actually about. Here the only observations in the database are the
/// ones the test — or the turn's own `rate_limit_event` / 429 line — put there.
async fn start_without_polling() -> (threads::ThreadManager, SocketAddr, tempfile::TempDir) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // CON-27, same reason as `start_full`: switching the POLLING off does not
    // change where a thread may RUN. `ensure_default` still registers the
    // machine `default` on boot, and nothing may be placed there any more, so
    // this fixture needs the same account threads OWNS — `aa-owned`, with its
    // own config dir under the temp dir. Without it every `threads.create`
    // here fails admission before the test reaches the gauge behaviour it is
    // actually about.
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        tmp.path().join("aa-owned").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        probe_interval_secs: 0,
        calibrate: false,
        edge_poll: false,
        accounts,
        ..Default::default()
    };
    let manager = build_manager(config).unwrap();
    let (addr, _handle) = threads::chassis::serve(std::sync::Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (manager, addr, tmp)
}

/// How many events of `kind` this thread's LEDGER holds — `Client::seen` only
/// answers for frames that happen to be buffered, which is not enough to prove
/// a prompt was delivered exactly once.
async fn ledger_count(c: &mut Client, id: &str, kind: &str) -> usize {
    let events = c
        .request_ok(
            &format!("threads/{}/history", id),
            json!({"kinds": [kind], "limit": 100}),
        )
        .await;
    events["events"].as_array().unwrap().len()
}

/// What `budgets/list` answered, read in-process off the manager. The wire read
/// had no caller and was RETIRED at R2.S3a; these tests still need to see every
/// key's config and meters, and the manager is right here, so they read the
/// same two public calls the retired arm made rather than a second door.
fn budgets_list(m: &threads::ThreadManager) -> Value {
    let accounts = m.account_pool();
    let mut out = Vec::new();
    for cfg in m.budgets.list_configs().unwrap() {
        match m.budgets.snapshot(&cfg.key, &accounts).unwrap() {
            Some(s) => out.push(s.to_json()),
            None => out.push(json!({ "key": cfg.key })),
        }
    }
    json!({ "budgets": out })
}

/// A daemon with a REAL multi-account pool (`extra` names additional accounts).
async fn start_with_accounts(
    extra: &[&str],
) -> (threads::ThreadManager, SocketAddr, tempfile::TempDir) {
    start_full(None, extra).await
}

async fn start_full(
    teardown: Option<&str>,
    extra_accounts: &[&str],
) -> (threads::ThreadManager, SocketAddr, tempfile::TempDir) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    // CON-27: the BASELINE placeable account, always registered. Every test in
    // this file used to serve on the machine `default` that `ensure_default`
    // adds on every boot; nothing may be placed there any more, so the fixture
    // registers one account threads OWNS — a name with its own config dir. It
    // sorts first, so it plays exactly the role `default` used to play in the
    // ranking, and the multi-account tests still get their sibling from
    // `extra_accounts`.
    accounts.insert(
        "aa-owned".to_string(),
        tmp.path().join("aa-owned").to_string_lossy().to_string(),
    );
    for name in extra_accounts {
        // A config dir that does not exist is fine here: the fake-claude
        // fixture ignores CLAUDE_CONFIG_DIR entirely.
        accounts.insert(
            name.to_string(),
            tmp.path().join(name).to_string_lossy().to_string(),
        );
    }
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        teardown_strategy: teardown.map(str::to_string),
        accounts,
        ..Default::default()
    };
    let manager = build_manager(config).unwrap();
    let (addr, _handle) = threads::chassis::serve(std::sync::Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    (manager, addr, tmp)
}

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
        let _welcome = c.next_frame().await;
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

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = format!("r{}", self.next_id);
        self.send_raw(json!({"type": "request", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let f = self.next_frame().await;
            if f["type"] == "response" {
                assert_eq!(f["correlate"], id.as_str());
                return f["outcome"].clone();
            }
            self.pending.push_back(f);
        }
    }

    async fn request_ok(&mut self, method: &str, params: Value) -> Value {
        let outcome = self.request(method, params).await;
        assert_eq!(outcome["type"], "success", "outcome: {outcome}");
        outcome["payload"].clone()
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

    async fn wait_event(&mut self, kind: &str) -> Value {
        self.wait_push(|f| f["type"] == "event" && f["event"]["kind"] == kind)
            .await
    }

    /// True if an event of `kind` has ALREADY been seen (never blocks).
    fn seen(&self, kind: &str) -> bool {
        self.bus.as_ref().is_some_and(|b| {
            b.held()
                .iter()
                .any(|f| f["type"] == "event" && f["event"]["kind"] == kind)
        })
    }

    /// Take in whatever arrives within `dur` without consuming it.
    async fn drain(&mut self, dur: Duration) {
        if let Some(b) = self.bus.as_mut() {
            b.absorb_for(dur).await;
        } else {
            tokio::time::sleep(dur).await;
        }
    }
}

fn settings(cwd: &std::path::Path) -> Value {
    json!({"cwd": cwd.to_string_lossy(), "model": "sonnet"})
}

/// The pid of the background grandchild the `bigburn` fixture mode launches
/// (its stand-in for a tool call / subagent execution). The fixture writes it
/// to the path the prompt named, so parallel tests never collide.
fn read_burn_child_pid(path: &std::path::Path) -> Option<i32> {
    for _ in 0..100 {
        if let Ok(t) = std::fs::read_to_string(path) {
            if let Ok(pid) = t.trim().parse::<i32>() {
                return Some(pid);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// `kill(pid, 0)` — EPERM still counts as alive.
fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Inject a calibration directly, standing in for the two `/usage` polls a
/// real account would take hours to produce. `tpp` tokens per percent ⇒
/// `100 * tpp` tokens is 100% of that axis.
///
/// `PRIOR_SAMPLES` samples, so the conservative prior is fully displaced and
/// the meter divides by exactly `tpp` (msg #1055 — see `gauge::blend_rate`).
fn calibrate(m: &threads::ThreadManager, account: &str, axis: &str, tpp: f64) {
    m.store
        .upsert_estimate(&EstimateRow {
            account: account.into(),
            axis: axis.into(),
            tokens_per_percent: tpp,
            sample_count: threads::gauge::PRIOR_SAMPLES as u64,
            last_obs_id: 0,
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rate_limit_event_is_first_classed_and_feeds_the_gauge() {
    // DEC-15 layer 1: the line the burn-in captured used to be swallowed as
    // `Unknown`. It is now a ledger event AND a gauge observation.
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let p = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = p["thread_id"].as_str().unwrap().to_string();
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "gaugeturn please"}),
    )
    .await;

    let ev = c.wait_event("rate_limit_event").await;
    assert_eq!(ev["event"]["rate_limit_type"], "seven_day");
    // Kept VERBATIM as the 0-1 fraction claude sends.
    assert_eq!(ev["event"]["utilization"], 0.73);
    assert_eq!(ev["event"]["status"], "allowed_warning");
    assert_eq!(ev["event"]["is_using_overage"], false);
    assert!(ev["event"]["resets_at_rfc3339"].is_string());
    // Nothing about it landed as `unknown`.
    assert!(!c.seen("unknown"));

    c.wait_event("turn_ended").await;

    // …and it normalized into the weekly axis at 0-100.
    let g = c.request_ok("gauge/status", json!({})).await;
    let weekly = &g["accounts"][0]["axes"]["weekly_all"];
    assert_eq!(weekly["observed_pct"], 73.0);
    assert_eq!(weekly["source"], "rate_limit_event");
    // One observation is not a calibration — no SAMPLE is fabricated, and the
    // readout still says so. What it no longer does is leave the denominator
    // null: msg #1055's prior fills it, which is what lets a percentage budget
    // gate before anything has been measured.
    assert_eq!(weekly["calibrated"], false);
    assert_eq!(weekly["calibration_samples"], 0);
    assert_eq!(weekly["confidence"], 0.0);
    assert_eq!(
        weekly["limit_tokens"].as_f64().unwrap(),
        threads::gauge::PRIOR_TOKENS_PER_PERCENT * 100.0
    );
}

#[tokio::test]
async fn usage_probe_covers_all_three_axes_and_calibrates_from_a_real_pair() {
    // DEC-15 layer 2 + DEC-12: the poll is the only gauge covering session,
    // weekly-all AND weekly-fable, and two polls with tokens between them
    // produce the first real limit estimate.
    //
    // **ON THE CLOCK SEAM, because this test MEASURES AN INTERVAL.** The
    // execution it records below has to have width: `started_at` and the ledger
    // row's own `ts` are two reads of the clock, and under an optimized build
    // they landed in the same microsecond — the window collapsed to
    // `start == end`, `calibrated` stayed `false`, and the assertion on it
    // failed. `ticking()` is real time that may not repeat, so the window has
    // width without the test depending on the binary being slow. One microsecond
    // of step, deliberately: `#[tokio::test]` is a current-thread runtime, so the
    // daemon's own tasks share this clock and must not be walked away from the
    // real wall time they compare reset windows against. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    // The probe only runs when something actually needs the gauge.
    c.request_ok(
        &format!("budgets/{}/set", "k"),
        json!({"session_pct": 20.0, "accounts": ["aa-owned"]}),
    )
        .await;

    let probed = c.request_ok("gauge/probe", json!({})).await;
    // Three axes on the ONE pool account. This asserted 6 — `aa-owned` and
    // `default` — on the reasoning that *"CON-27 took `default` out of
    // PLACEMENT, not out of METERING"*. That reasoning is overruled, and it was
    // the most expensive version of the split: a probe is not a table read, it
    // SPAWNS `claude`, and with no config dir it spawned against `~/.claude` —
    // the operator's own login — on a timer. *"Threads should never access my
    // default account."* One pool account, three axes, three observations.
    assert_eq!(probed["observations"], 3);
    // **THE READINGS COME FROM THE GAUGE, NOT FROM THE COMMAND'S REPLY.**
    // This used to read `probed["accounts"][0]["axes"]` — the gauge state off
    // the reply to `gauge.probe`, which is the second source rule 4 deletes
    // (`docs/shapes.md` 2.4). `observations` above STAYS: that is a fact about
    // this call, not about the gauge, and nothing else reports it.
    let state = m.budgets.gauge_state(&m.account_pool()).unwrap();
    let axes = &state[0]["axes"];
    assert_eq!(axes["session"]["observed_pct"], 12.0);
    assert_eq!(axes["weekly_all"]["observed_pct"], 34.0);
    assert_eq!(axes["weekly_fable"]["observed_pct"], 56.0);
    assert_eq!(axes["session"]["calibrated"], false); // one sample is not a pair

    // The fixture always reports the SAME percentages, so a second poll moves
    // nothing and NO estimate may be invented (CCW INTENT #202).
    c.request_ok("gauge/probe", json!({})).await;
    let g = c.request_ok("gauge/status", json!({})).await;
    assert_eq!(g["accounts"][0]["axes"]["session"]["calibrated"], false);

    // Real consumption then lands on the account…
    let p = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = p["thread_id"].as_str().unwrap().to_string();
    // `started_at` brackets this as an EXECUTION interval between the two
    // polls — the overlap-window method (msg #620) needs one, not just
    // tokens on the ledger; `runtime.rs` stamps this live from
    // `running_turns` before it clears the row.
    m.store
        .record_usage(&threads::store::UsageRow {
            thread_id: id,
            account: Some("aa-owned".into()),
            model: "claude-sonnet-5".into(),
            input_tokens: 1_000_000,
            started_at: Some(threads::store::now_rfc3339()),
            ..Default::default()
        })
        .unwrap();

    // …and a genuine percent delta on the same window IS a calibration sample.
    m.budgets
        .observe(
            "aa-owned",
            threads::gauge::Axis::Session,
            22.0,
            m.store
                .recent_observations("aa-owned", "session", 1)
                .unwrap()[0]
                .resets_at
                .clone(),
            "usage_poll",
        )
        .unwrap();
    let g = c.request_ok("gauge/status", json!({})).await;
    let session = &g["accounts"][0]["axes"]["session"];
    assert_eq!(session["calibrated"], true);
    assert_eq!(session["calibration_samples"], 1);

    // 1M tokens per 10 percent ⇒ 100k/percent ⇒ 10M tokens is 100%, as the
    // stored SAMPLE. What the gate divides by is the blend: msg #1055 / audit
    // IS-50, the first accepted sample no longer arms the meter on its own, so
    // the effective denominator is still mostly the conservative prior and
    // walks towards the sample over the next three.
    let est = m.store.get_estimate("aa-owned", "session").unwrap().unwrap();
    assert!((est.tokens_per_percent - 100_000.0).abs() < 100.0);
    let effective = session["limit_tokens"].as_f64().unwrap() / 100.0;
    let expected =
        threads::gauge::blend_rate(threads::gauge::PRIOR_TOKENS_PER_PERCENT, Some((est.tokens_per_percent, 1)));
    assert!((effective - expected).abs() < 1.0, "{effective} != {expected}");
    assert!(
        effective > threads::gauge::PRIOR_TOKENS_PER_PERCENT && effective < est.tokens_per_percent,
        "the blend must sit strictly between the prior and the sample: {effective}"
    );
    assert_eq!(session["confidence"], 0.25);
}

/// `gauge.probe` means "poll now", and it either polls or says why it did not.
///
/// It used to be a SILENT no-op in two different ways: `probe_interval_secs = 0`
/// installed `NoProbe`, and the background sweep's "does anything need the
/// gauge?" precondition applied to the explicit call too. Either way the
/// operator got `observations: 0` back — no reading, no error, no reason.
/// A poll that quietly does nothing is worse than one that costs an
/// invocation, and worse still because calibration can only ever start from a
/// reading.
#[tokio::test]
async fn an_explicit_gauge_probe_polls_or_explains_itself() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        // ONE POOL ACCOUNT. It used to be none — the probe still had a target
        // because `ensure_default` registered the machine default on every
        // boot, i.e. this test's target WAS the operator's own login. The
        // daemon no longer creates that row, so the target is stated.
        accounts: [(
            "aa-owned".to_string(),
            tmp.path().join("aa-owned").to_string_lossy().to_string(),
        )]
        .into_iter()
        .collect(),
        // The exact configuration that used to disable the explicit call.
        probe_interval_secs: 0,
        // Calibration polling OFF, so this test still sees the "nothing needs a
        // reading" arm. With it ON (the default — see
        // `the_background_sweep_polls_because_calibration_wants_a_reading`) the
        // sweep polls, which is the whole point of the flag.
        calibrate: false,
        edge_poll: false,
        // …and BARRING off. `gauge_bar_pct` defaults to 100, which makes the
        // gauge an enforcement mechanism in its own right and therefore always
        // needed; this arm of the test is about the case where NOTHING needs a
        // reading, so it has to switch that off too. The other arm is below.
        gauge_bar_pct: 0.0,
        ..Default::default()
    };
    let manager = build_manager(config).unwrap();
    let (addr, _handle) = threads::chassis::serve(std::sync::Arc::new(manager.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut c = Client::connect(addr).await;

    // No budget key, no threshold, background probing off — and it still
    // polls, because the operator asked for a reading.
    let probed = c.request_ok("gauge/probe", json!({})).await;
    assert_eq!(
        probed["observations"], 3,
        "an explicit probe must poll regardless of config: {probed:#}"
    );
    // Same correction as above: the reading is read from the gauge.
    let state = manager.budgets.gauge_state(&manager.account_pool()).unwrap();
    assert_eq!(state[0]["axes"]["session"]["observed_pct"], 12.0);

    // The BACKGROUND sweep still refuses when NOTHING needs a reading — no
    // budget, no threshold, no barring, and calibration polling switched off —
    // and it says so rather than reporting a bare zero.
    let skipped = manager
        .probe_accounts_once(false)
        .await
        .expect_err("nothing needs the gauge, so the timed sweep must not poll");
    assert!(
        skipped.contains("no budget key, no threshold"),
        "the skip must name its reason: {skipped}"
    );
    // Same manager, forced: polls.
    assert_eq!(manager.probe_accounts_once(true).await.unwrap(), 3);
}

/// **THE POLLER MAY NOT STOP, BECAUSE THE POLLER IS NOW A SAFETY MECHANISM.**
///
/// The other arm, and the one that matters on the live deployment. Nothing
/// there enforces and no thread carried a threshold, so the ONLY reason the
/// background sweep ever ran was incomplete calibration — and when the last
/// axis of the last account calibrates, the sweep refuses and the poller stops.
/// That was survivable while a `/usage` reading was merely an input to a budget.
/// It is not survivable now that `probe_account` writes `accounts.limit_until`
/// on an exhausted reading: a sweep that does not run is a bar that is never
/// written, which is exactly the 2026-08-14 silence.
///
/// Every reason to poll is off here — no budget, no threshold, calibration
/// disabled — and the sweep polls anyway, on the strength of `gauge_bar_pct`
/// alone.
#[tokio::test]
async fn the_background_sweep_polls_because_the_gauge_can_bar() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // ONE POOL ACCOUNT, registered explicitly. It used to be none: the sweep
    // still had something to poll because `ensure_default` put the machine
    // default in the registry on every boot, which is precisely the thing that
    // made the probe spawn against `~/.claude`. The daemon no longer creates
    // that row, so a machine with an empty pool now correctly polls nothing —
    // and this test has to say what it is polling.
    let manager = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts: [(
            "aa-owned".to_string(),
            tmp.path().join("aa-owned").to_string_lossy().to_string(),
        )]
        .into_iter()
        .collect(),
        probe_interval_secs: 0,
        calibrate: false,
        edge_poll: false,
        // The default, stated: bar at a measured 100%.
        gauge_bar_pct: 100.0,
        ..Default::default()
    })
    .unwrap();

    assert_eq!(
        manager
            .probe_accounts_once(false)
            .await
            .expect("the gauge can bar, so the gauge is needed, so the sweep must poll"),
        3,
        "all three axes"
    );
}

#[tokio::test]
async fn a_threshold_parks_the_turn_and_it_resumes_when_usage_clears() {
    // DEC-6/DEC-10 + CON-4: a gate trip means the thread WAITS, the work is
    // parked (never dropped), and it goes out by itself once the gauge clears.
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let mut s = settings(tmp.path());
    s["thresholds"] = json!({"fable": 60.0});
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // The only account in the pool is over its fable threshold.
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

    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "threshold");
    assert!(parked["event"]["reason"]
        .as_str()
        .unwrap()
        .contains("weekly_fable"));
    assert_eq!(parked["event"]["message_ids"], json!([1]));

    // The prompt is still QUEUED, not consumed — CCW dropped it here.
    c.drain(Duration::from_millis(400)).await;
    assert!(!c.seen("turn_started"), "a parked turn must not spawn");
    let q = queue_rows(addr, &id, "waiting").await;
    assert_eq!(q[0]["message_id"], 1);
    assert_eq!(q[0]["text"], "please do the work");

    // The window rolls over / the operator's usage recovers.
    m.budgets
        .observe(
            "aa-owned",
            threads::gauge::Axis::WeeklyFable,
            9.0,
            None,
            "usage_poll",
        )
        .unwrap();

    let resumed = c.wait_event("turn_resumed").await;
    assert!(resumed["event"]["reason"].as_str().unwrap().contains("threshold"));
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
    c.wait_event("turn_ended").await;
}

#[tokio::test]
async fn a_budget_kill_tears_down_the_execution_and_parks_the_prompt() {
    // `on_trip: "kill"` + DIR-19 + CON-4 + D-8: the turn dies, its whole
    // process group dies with it, and the operator's prompt is preserved.
    // (DEC-13's default-KILL is superseded by round-7 item 3 — FINISH is the
    // default — so the kill is set EXPLICITLY below, as the toggle it is now.)
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    // 1% of the session axis = 1000 tokens; the budget allows 1%.
    calibrate(&m, "aa-owned", "session", 1000.0);
    calibrate(&m, "aa-owned", "weekly_all", 1000.0);
    c.request_ok(
        &format!("budgets/{}/set", "tiny"),
        json!({"session_pct": 1.0, "on_trip": "kill",
               "accounts": ["aa-owned"]}),
    )
    .await;

    let mut s = settings(tmp.path());
    s["budget_key"] = json!("tiny");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    let burnfile = tmp.path().join("burnchild.pid");
    let prompt_text = format!(
        "bigburn: consume everything burnfile={}",
        burnfile.display()
    );
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": prompt_text}),
    )
    .await;

    c.wait_event("turn_started").await;
    let tripped = c.wait_event("budget_tripped").await;
    assert_eq!(tripped["event"]["budget_key"], "tiny");
    assert_eq!(tripped["event"]["axis"], "session");
    assert_eq!(tripped["event"]["action"], "kill");
    assert!(tripped["event"]["consumed_pct"].as_f64().unwrap() > 1.0);

    let killed = c.wait_event("turn_killed").await;
    assert_eq!(killed["event"]["budget_key"], "tiny");
    // The teardown strategy is on the record so M-9 can compare strategies.
    assert_eq!(killed["event"]["strategy"], "process_group");

    // The child really died — the fixture would otherwise sleep 5s and emit a
    // `result_turn`. `turn_ended` arrives well inside that window.
    let ended = tokio::time::timeout(Duration::from_secs(4), c.wait_event("turn_ended"))
        .await
        .expect("the killed child must exit long before its 5s sleep elapses");
    assert_ne!(ended["event"]["exit_code"], 0);
    assert!(!c.seen("result_turn"), "the killed turn never completed");

    // D-8: the GRANDCHILD died too. The fixture launched a 30s background
    // process standing in for a tool call / subagent execution; the
    // process-group teardown must have swept it, or it would be orphaned.
    let child_pid = read_burn_child_pid(&burnfile).expect("fixture recorded its grandchild pid");
    let mut alive = true;
    for _ in 0..40 {
        if !pid_alive(child_pid) {
            alive = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !alive,
        "grandchild pid {child_pid} survived the teardown — orphaned child process (D-8)"
    );

    // CON-4: the prompt is parked verbatim, not dropped.
    let parked_ev = c.wait_event("turn_parked").await;
    assert_eq!(parked_ev["event"]["cause"], "budget");
    let parked = m.store.get_parked_turn(&id).unwrap().expect("parked row");
    assert!(parked.prompt.contains("bigburn: consume everything"));
    assert_eq!(parked.message_ids, vec![1]);
    assert_eq!(parked.budget_key.as_deref(), Some("tiny"));

    // budgets.get reports the trip and the per-account breakdown.
    let b = c.request_ok(&format!("budgets/{}/get", "tiny"), json!({})).await;
    assert_eq!(b["tripped"], true);
    assert_eq!(b["tripped_axis"], "session");
    assert_eq!(b["session"]["status"], "exhausted");
    assert_eq!(b["session"]["accounts"][0]["account"], "aa-owned");
    assert!(b["session"]["accounts"][0]["tokens"].as_u64().unwrap() >= 5_000_000);
    assert_eq!(b["threads"], json!([id]));

    // DEC-11: raising the limit is just a config edit — and it unparks.
    c.request_ok(
        &format!("budgets/{}/set", "tiny"),
        json!({"session_pct": 1_000_000.0}),
    )
    .await;
    c.wait_event("turn_resumed").await;
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
}

/// A CASH budget can now bite mid-turn — the gap that made the mid-execution
/// kill unreachable on 2026-07-29.
///
/// Before the per-message pricing, `MessageUsage` carried token counts but no
/// cost, so the cash meter could not move until `result_turn`. A cash budget
/// therefore always bit at the NEXT admission, never during the execution, and
/// `on_trip: kill` had nothing left to kill by the time it fired. The three
/// percentage axes could not cover for it either: without a calibration sample
/// they report `calibrating` and never gate. That left no reachable mid-turn
/// trip at all.
///
/// This is the in-process proof that the path exists, costing nothing: the
/// fixture streams one enormous message BEFORE its work finishes, the estimate
/// prices it past the limit, and the execution is torn down where it stands.
#[tokio::test]
async fn a_cash_budget_trips_and_kills_during_the_execution() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    // Half a cent, killing. The fixture's `bigburn` message is 5M fable input
    // tokens — dollars in the tens — so the estimate crosses the line on the
    // FIRST streamed message, long before the turn would end.
    c.request_ok(
        &format!("budgets/{}/set", "cashkill"),
        json!({"cash_usd": 0.005, "on_trip": "kill",
               "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("cashkill");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    let burnfile = tmp.path().join("cashburn.pid");
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({               "text": format!("bigburn: spend it all burnfile={}", burnfile.display())}),
    )
    .await;

    c.wait_event("turn_started").await;
    let tripped = c.wait_event("budget_tripped").await;
    assert_eq!(tripped["event"]["axis"], "cash", "{tripped:#}");
    assert_eq!(tripped["event"]["action"], "kill");
    assert!(tripped["event"]["consumed_pct"].as_f64().unwrap() > 0.005);

    // Killed DURING the execution: the fixture sleeps 5s before its result, so
    // a turn that ends inside that window was torn down, not finished.
    let killed = c.wait_event("turn_killed").await;
    assert_eq!(killed["event"]["budget_key"], "cashkill");
    assert_eq!(killed["event"]["strategy"], "process_group");
    let ended = tokio::time::timeout(Duration::from_secs(4), c.wait_event("turn_ended"))
        .await
        .expect("a mid-turn kill must land well inside the fixture's 5s sleep");
    assert_ne!(ended["event"]["exit_code"], 0);
    assert!(!c.seen("result_turn"), "the killed turn never completed");

    // No orphans: the grandchild the fixture spawned went with it (D-8).
    let child_pid = read_burn_child_pid(&burnfile).expect("fixture recorded its grandchild pid");
    let mut alive = true;
    for _ in 0..40 {
        if !pid_alive(child_pid) {
            alive = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive, "grandchild pid {child_pid} survived a cash kill (D-8)");

    // CON-4: the prompt is parked, never lost.
    let parked_ev = c.wait_event("turn_parked").await;
    assert_eq!(parked_ev["event"]["cause"], "budget");
    let parked = m.store.get_parked_turn(&id).unwrap().expect("parked row");
    assert!(parked.prompt.contains("bigburn: spend it all"));
    assert_eq!(parked.budget_key.as_deref(), Some("cashkill"));

    assert!(m.store.get_parked_turn(&id).unwrap().is_some());
}

/// The kill/park LIVELOCK, and the latch that stops it.
///
/// A budget kill necessarily fires on the live mid-turn ESTIMATE. The turn
/// epilogue then reconciles that estimate against the authoritative `costUSD`
/// the torn-down turn flushes on its way out — and when the estimate over-shot,
/// the meter drops back UNDER the limit. The prompt that was parked one
/// millisecond earlier is then admissible again, redispatches, over-shoots, and
/// is killed again.
///
/// Observed live on 2026-07-29: eight kill → park → resume → kill cycles in 29
/// seconds, each one spawning a real billable claude turn, still going when the
/// daemon was stopped by hand. Enforcement that spends money in a loop is worse
/// than no enforcement, so a kill LATCHES until the operator moves the limit.
#[tokio::test]
async fn a_killed_turn_stays_parked_instead_of_relaunching_itself() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    c.request_ok(
        &format!("budgets/{}/set", "latch"),
        json!({"cash_usd": 0.01, "on_trip": "kill",
               "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("latch");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "killcheap: overshoot then flush a cheap result"}),
    )
    .await;

    // The estimate trips and kills…
    let tripped = c.wait_event("budget_tripped").await;
    assert_eq!(tripped["event"]["action"], "kill");
    c.wait_event("turn_killed").await;
    c.wait_event("turn_ended").await;
    let parked_ev = c.wait_event("turn_parked").await;
    assert_eq!(parked_ev["event"]["cause"], "budget");

    // …and the authoritative cost then reconciles the meter back UNDER the
    // limit. This is the exact state that used to relaunch the turn.
    let b = c.request_ok(&format!("budgets/{}/get", "latch"), json!({})).await;
    let consumed = b["cash"]["consumed_usd"].as_f64().unwrap();
    assert!(
        consumed < 0.01,
        "this test is only meaningful while reconciliation leaves the meter \
         under the limit (got {consumed}) — otherwise the meter alone would hold it"
    );

    // The prompt nonetheless stays put.
    c.drain(Duration::from_millis(1500)).await;
    assert!(
        !c.seen("turn_resumed"),
        "a killed turn must not relaunch itself — that is the livelock"
    );
    assert!(m.store.get_parked_turn(&id).unwrap().is_some());

    // The operator's existing escape hatch still releases it (DEC-11): move
    // the limit and the parked prompt goes out, message ids intact.
    c.request_ok(&format!("budgets/{}/set", "latch"), json!({"cash_usd": 500.0}))
        .await;
    c.wait_event("turn_resumed").await;
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
}

#[tokio::test]
async fn the_child_only_teardown_strategy_orphans_grandchildren() {
    // The comparison arm for M-9, and the proof that the process-group
    // assertion above has teeth. `child_only` signals ONLY claude, so the
    // grandchild it spawned is reparented to init and keeps running — the
    // exact residue D-8 names. Kept as a runnable demonstration of the seam,
    // not as an endorsement.
    let (m, addr, tmp) = start_with_teardown(Some("child_only")).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    calibrate(&m, "aa-owned", "session", 1000.0);
    calibrate(&m, "aa-owned", "weekly_all", 1000.0);
    c.request_ok(
        &format!("budgets/{}/set", "tiny2"),
        json!({"session_pct": 1.0, "on_trip": "kill",
               "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("tiny2");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();
    let burnfile = tmp.path().join("burnchild.pid");
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({               "text": format!("bigburn under child_only burnfile={}", burnfile.display())}),
    )
    .await;

    let killed = c.wait_event("turn_killed").await;
    assert_eq!(killed["event"]["strategy"], "child_only");
    c.wait_event("turn_ended").await;

    let child_pid = read_burn_child_pid(&burnfile).expect("fixture recorded its grandchild pid");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        pid_alive(child_pid),
        "child_only is expected to ORPHAN the grandchild; if this now passes \
         the strategy changed and M-9's comparison needs revisiting"
    );
    // Do not leave the orphan running for the rest of the suite.
    unsafe { libc::kill(child_pid, libc::SIGKILL) };

    // Even under the weaker strategy the prompt is still parked (CON-4).
    assert!(m.store.get_parked_turn(&id).unwrap().is_some());
}

/// SUPERSEDES `an_uncalibrated_budget_never_gates_and_says_so`.
///
/// That test asserted D-3/CON-5's old answer: an axis with no accepted sample
/// reported `calibrating`, priced its spend at zero and never gated — which was
/// honest, and left every percentage budget decorative until a sample happened
/// to land. Msg #1055 replaced the refusal with a conservative prior. The same
/// key now bites on the first turn, over the wire AUI actually reads, with
/// nothing spent on calibration.
#[tokio::test]
async fn an_uncalibrated_budget_gates_on_the_prior_and_says_so() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    c.request_ok(
        &format!("budgets/{}/set", "nocal"),
        json!({"session_pct": 0.000_001, "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("nocal");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    c.wait_event("turn_ended").await;

    let b = c.request_ok(&format!("budgets/{}/get", "nocal"), json!({})).await;
    let tokens = b["session"]["consumed_tokens"].as_u64().unwrap();
    assert!(tokens > 0);
    // The percent is priced, not fabricated as zero: tokens over the prior.
    let pct = b["session"]["consumed_pct"].as_f64().unwrap();
    assert!(
        (pct - tokens as f64 / threads::gauge::PRIOR_TOKENS_PER_PERCENT).abs() < 1e-9,
        "{pct} from {tokens} tokens"
    );
    assert_eq!(b["session"]["status"], "exhausted");
    assert_eq!(b["tripped"], true);
    assert_ne!(b["session"]["accounts"][0]["pct_of_account"], Value::Null);
    // And no sample was bought to get there — the estimate table is empty.
    assert!(m.store.get_estimate("aa-owned", "session").unwrap().is_none());

    // The next turn is refused by it, which is the whole point of un-gating.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "again"}))
        .await;
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "budget");
}

#[tokio::test]
async fn keys_are_shareable_repointable_and_outlive_their_threads() {
    // DEC-4 + DEC-14, over the WS surface AUI will actually use.
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    c.request_ok(
        &format!("budgets/{}/set", "project-a"),
        json!({"session_pct": 20.0, "weekly_pct": 10.0,
               "accounts": ["aa-owned"]}),
    )
    .await;

    let a = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();

    // INTENT 72b: a thread that named no key is not unkeyed, it INHERITS the
    // fleet-wide default. "Every single thread has to be associated with the
    // budget" — met literally, and met by inheritance rather than by refusing
    // to create the thread.
    let ft = c
        .request_ok(&format!("threads/{}/budget_get", a), json!({}))
        .await;
    assert_eq!(ft["budget_key"], threads::budgets::DEFAULT_BUDGET_KEY);

    // Several threads may SHARE one key — the whole point of keying.
    c.request_ok(
        &format!("threads/{}/budget_assign", a),
        json!({"key": "project-a"}),
    )
    .await;
    c.request_ok(
        &format!("threads/{}/budget_assign", b),
        json!({"key": "project-a"}),
    )
    .await;
    let got = c.request_ok(&format!("budgets/{}/get", "project-a"), json!({})).await;
    let mut users: Vec<String> = got["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    users.sort();
    let mut expect = vec![a.clone(), b.clone()];
    expect.sort();
    assert_eq!(users, expect);

    // Re-pointing mid-life is legal…
    c.request_ok(
        &format!("budgets/{}/set", "project-b"),
        json!({"session_pct": 40.0, "accounts": ["aa-owned"]}),
    )
    .await;
    c.request_ok(
        &format!("threads/{}/budget_assign", b),
        json!({"key": "project-b"}),
    )
    .await;
    assert_eq!(
        c.request_ok(&format!("threads/{}/budget_get", b), json!({}))
            .await["budget_key"],
        "project-b"
    );
    // …and so is unkeying (`key: null` ⇒ no budget).
    c.request_ok(&format!("threads/{}/budget_assign", b), json!({"key": null}))
        .await;
    assert_eq!(
        c.request_ok(&format!("threads/{}/budget_get", b), json!({}))
            .await["budget_key"],
        Value::Null
    );

    // Keys are listed independently of threads and outlive them.
    let list = budgets_list(&_m);
    let keys: Vec<&str> = list["budgets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["project-a", "project-b"]);
    assert_eq!(list["budgets"][0]["config"]["session_pct"], 20.0);
    assert_eq!(list["budgets"][0]["config"]["weekly_pct"], 10.0);
    // ROUND-7 item 3: the default a key gets when it does not say so is now
    // FINISH — kill is a toggle an application opts into.
    assert_eq!(list["budgets"][0]["config"]["on_trip"], "finish");
    // Round-7 item 6: the scope it was created with, never widened.
    assert_eq!(list["budgets"][0]["config"]["accounts"], json!(["aa-owned"]));

    // Q-34 (msg #751): there is no deleting. The old `budgets.delete` refuses
    // and points at the replacement, and the key it named is untouched.
    let refused = c
        .request(&format!("budgets/{}/delete", "project-b"), json!({}))
        .await;
    assert_eq!(refused["type"], "error", "{refused}");
    let msg = refused["error"].as_str().unwrap_or_default().to_string();
    assert!(msg.contains("never deleted"), "{msg}");
    assert!(msg.contains(&format!("budgets/{}/disable", "project-b")), "{msg}");
    let still_there = c
        .request_ok(&format!("budgets/{}/get", "project-b"), json!({}))
        .await;
    assert_eq!(still_there["config"]["session_pct"], 40.0);
    assert_eq!(still_there["disabled"], false);
}

#[tokio::test]
async fn a_parked_turn_survives_a_daemon_restart() {
    // CON-4 across a reboot: the killed prompt is durable, like a retry row.
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let p = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    m.store
        .upsert_parked_turn(&threads::store::ParkedTurnRow {
            thread_id: id.clone(),
            prompt: "a prompt parked by a previous daemon life".into(),
            message_ids: vec![1],
            attempt: 1,
            reason: "budget exhausted".into(),
            budget_key: Some("gone".into()),
            parked_at: chrono::Utc::now().to_rfc3339(),
        })
        .unwrap();

    m.recover().await;

    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
    c.wait_event("turn_ended").await;
    // Delivered exactly once, then cleared.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(m.store.get_parked_turn(&id).unwrap().is_none());
}

#[tokio::test]
async fn usage_cap_fraction_is_superseded_by_budget_keys() {
    // DEC-19 / D-10: exactly ONE enforcement point. The old seam is gone from
    // the settings blob entirely — a caller that still sets it is ignored
    // rather than silently enforced by a second mechanism.
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    let mut s = settings(tmp.path());
    s["retry_policy"] = json!({"usage_cap_fraction": 0.2, "max_attempts": 3});
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    let got = stored_thread(&m, &id);
    assert!(
        got["settings"]["retry_policy"]["usage_cap_fraction"].is_null(),
        "usage_cap_fraction must not round-trip: {}",
        got["settings"]["retry_policy"]
    );
    assert_eq!(got["settings"]["retry_policy"]["max_attempts"], 3);
    // The spend cap it expressed is now a budget key — and since INTENT 72b
    // every thread carries one, so this is the inherited default rather than
    // nothing at all.
    assert_eq!(
        got["settings"]["budget_key"],
        threads::budgets::DEFAULT_BUDGET_KEY
    );
}

#[tokio::test]
async fn thresholds_are_live_editable_and_compose_with_budgets() {
    // §15.3: budgets and thresholds compose with AND — and both are config,
    // changeable at runtime (DEC-11).
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    calibrate(&m, "aa-owned", "session", 1000.0);
    calibrate(&m, "aa-owned", "weekly_all", 1000.0);
    c.request_ok(
        &format!("budgets/{}/set", "k"),
        json!({"session_pct": 90.0, "accounts": ["aa-owned"]}),
    )
    .await;
    m.budgets
        .observe(
            "aa-owned",
            threads::gauge::Axis::Session,
            95.0,
            None,
            "usage_poll",
        )
        .unwrap();

    let mut s = settings(tmp.path());
    s["budget_key"] = json!("k");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // Budget fine, threshold not: the AND still blocks.
    c.request_ok(
        &format!("threads/{}/update", id),
        json!({"patch": {"thresholds": {"session": 50.0}}}),
    )
    .await;
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "work"}))
        .await;
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "threshold");

    let ft = c
        .request_ok(&format!("threads/{}/budget_get", id), json!({}))
        .await;
    assert_eq!(ft["thresholds"]["session"], 50.0);
    assert_eq!(ft["budget"]["session"]["limit_pct"], 90.0);
    assert_eq!(ft["parked"]["reason"].is_string(), true);

    // Clearing the threshold releases it immediately — no tick needed.
    c.request_ok(
        &format!("threads/{}/update", id),
        json!({"patch": {"thresholds": {}}}),
    )
    .await;
    c.wait_event("turn_resumed").await;
    c.wait_event("turn_started").await;
    c.wait_event("turn_ended").await;
}

// ─────────────────────────────────────────────────────────────────────────────
// The operator's round-7 corrections, end to end over the WS surface.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_internal_failure_fails_closed_and_budgeting_off_is_the_way_out() {
    // ROUND-7 item 1, the whole of it. The previous build admitted the turn on
    // an internal budgets error; the operator rejected that: "If it fails, then
    // nothing runs except for threads that have all budgeting turned off ...
    // I would turn off budgeting on a particular thread, bypass that module,
    // fix the module, and then resume behavior."
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    c.request_ok(
        &format!("budgets/{}/set", "closed"),
        json!({"session_pct": 50.0, "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("closed");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // Break the module from underneath: its ledger is gone.
    m.store.execute_sql("DROP TABLE budget_charges").unwrap();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;

    // LOUD: a first-class failure event, not a log line nobody reads.
    let failed = c.wait_event("budgets_failed").await;
    assert_eq!(failed["event"]["phase"], "admission");
    assert_eq!(failed["event"]["outcome"], "parked");
    assert!(failed["event"]["error"].as_str().unwrap().contains("budget_charges"));

    // CLOSED: it does not run…
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "internal");
    assert!(parked["event"]["reason"]
        .as_str()
        .unwrap()
        .contains("budgeting_off"));
    c.drain(Duration::from_millis(500)).await;
    assert!(!c.seen("turn_started"), "a failed module must not admit");

    // …and CON-4 still holds: the work is parked, not dropped.
    let q = queue_rows(addr, &id, "waiting").await;
    assert_eq!(q[0]["text"], "hello");

    // A broken module re-checked every resume tick must not flood the ledger.
    let events = c
        .request_ok(
            &format!("threads/{}/history", id),
            json!({"kinds": ["budgets_failed"], "limit": 50}),
        )
        .await;
    assert_eq!(events["events"].as_array().unwrap().len(), 1);

    // THE REMEDY PATH: bypass the module on this thread and it runs again.
    c.request_ok(
        &format!("threads/{}/update", id),
        json!({"patch": {"budgeting_off": true}}),
    )
    .await;
    c.wait_event("turn_resumed").await;
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
    c.wait_event("turn_ended").await;

    // The bypass is visible on the thread…
    let got = stored_thread(&m, &id);
    assert_eq!(got["settings"]["budgeting_off"], true);
    // …and the query surface is honest about the module still being broken
    // rather than answering with a meter it cannot compute.
    let outcome = c
        .request(&format!("threads/{}/budget_get", id), json!({}))
        .await;
    assert_eq!(outcome["type"], "error");
}

#[tokio::test]
async fn a_trip_lets_the_execution_finish_by_default_and_gates_the_next_turn() {
    // ROUND-7 item 3: "Our default strategy is going to be to let a thread
    // finish." DEC-13's default-kill is superseded; the teardown seam stays.
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    // 1% of the session axis = 1 token, so the first assistant message trips it.
    calibrate(&m, "aa-owned", "session", 1.0);
    c.request_ok(
        &format!("budgets/{}/set", "finisher"),
        json!({"session_pct": 1.0, "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("finisher");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;

    let tripped = c.wait_event("budget_tripped").await;
    assert_eq!(tripped["event"]["action"], "finish");
    let ended = c.wait_event("turn_ended").await;
    assert_eq!(ended["event"]["exit_code"], 0);
    // The execution ran to completion: no teardown, and a real result.
    assert!(!c.seen("turn_killed"), "the default must not kill");
    assert!(c.seen("result_turn"), "the execution was allowed to finish");
    assert!(m.store.get_parked_turn(&id).unwrap().is_none());

    // The NEXT turn is where an exhausted budget bites.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "again"}))
        .await;
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "budget");
    assert_eq!(parked["event"]["budget_key"], "finisher");
}

#[tokio::test]
async fn the_cash_axis_is_dollars_pooled_across_accounts() {
    // ROUND-7 item 5: "every time there's a usage limit, including the cash
    // usage limit, we have thresholds and budgets on it. But the cash ones are
    // ... based off of dollars ... if we say $20, that's not $20 per account,
    // that's $20 across all the accounts."
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    // The fixture's turn costs $0.01; the budget allows half a cent.
    c.request_ok(
        &format!("budgets/{}/set", "cash"),
        json!({"cash_usd": 0.005, "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("cash");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    let tripped = c.wait_event("budget_tripped").await;
    assert_eq!(tripped["event"]["axis"], "cash");
    c.wait_event("turn_ended").await;

    let b = c.request_ok(&format!("budgets/{}/get", "cash"), json!({})).await;
    assert_eq!(b["tripped_axis"], "cash");
    assert_eq!(b["cash"]["unit"], "dollars");
    assert_eq!(b["cash"]["limit_usd"], 0.005);
    assert_eq!(b["cash"]["status"], "exhausted");
    assert!((b["cash"]["consumed"].as_f64().unwrap() - 0.01).abs() < 1e-9);
    // No estimate was needed anywhere: dollars are measured, not calibrated.
    assert_eq!(b["cash"]["accounts"][0]["limit_tokens"], Value::Null);
    assert_eq!(b["metered_accounts"], json!(["aa-owned"]));

    // …and it gates the next turn, in dollars.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "again"}))
        .await;
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "budget");
    assert!(parked["event"]["reason"].as_str().unwrap().contains('$'));
}

/// The runtime half of the double-charge fix (the defect observed live on
/// 2026-07-29: 22,462 true tokens metered as 44,862, every meter ~2x high).
///
/// Both halves are exercised here end to end:
///
/// * the DEDUPE — the fixture repeats one message's `usage` payload across two
///   `assistant` lines, exactly as real claude does, and the meter must count
///   those 1,000,000 tokens once;
/// * the SIGNED RECONCILIATION — the turn epilogue lands the meter on the
///   authoritative `result_turn` totals whichever way the provisional stream
///   charge erred. It could only ever add before (`saturating_sub` clamped an
///   over-charge to zero), so an inflated meter stayed inflated forever.
#[tokio::test]
async fn a_repeated_usage_line_is_charged_once_and_reconciles_to_the_authority() {
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    // A budget big enough that nothing trips — this test is about the NUMBER
    // on the meter, not about enforcement.
    c.request_ok(
        &format!("budgets/{}/set", "dupe"),
        json!({"cash_usd": 100.0, "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("dupe");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "dupeusage please"}),
    )
    .await;
    c.wait_event("turn_ended").await;

    let b = c.request_ok(&format!("budgets/{}/get", "dupe"), json!({})).await;
    // The stream reported 1,000,000 tokens TWICE for one message; the
    // authoritative aggregate says 1,000,000. Charging per line would read
    // 2,000,000 here.
    assert_eq!(
        b["session"]["consumed_tokens"], 1_000_000,
        "a repeated usage payload must be charged once: {b:#}"
    );
    // And the cash meter equals the authoritative costUSD to the cent — the
    // mid-turn ESTIMATE was reconciled away, not left on the ledger.
    let cash = b["cash"]["consumed_usd"].as_f64().unwrap();
    assert!(
        (cash - 0.5).abs() < 1e-9,
        "cash must reconcile to the authoritative $0.50, got {cash}: {b:#}"
    );
}

#[tokio::test]
async fn every_axis_carries_a_budget_including_weekly_fable() {
    // ROUND-7 item 4: "There's three different limits ... We need thresholds
    // on all three of those usage limits and budgets on all three."
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    // The fixture answers as `claude-fable-5`, so its usage lands on the fable
    // axis as well as the two account-wide ones.
    calibrate(&m, "aa-owned", "weekly_fable", 1.0);
    let ack = c
        .request_ok(
            &format!("budgets/{}/set", "allaxes"),
            json!({"session_pct": 90.0, "weekly_pct": 90.0,
                   "fable_pct": 1.0, "cash_usd": 100.0, "accounts": ["aa-owned"]}),
        )
        .await;
    // **THE SET ANSWERS AN ACKNOWLEDGEMENT** (rule 2.4) — it used to answer the
    // snapshot, which is state. The snapshot is READ, through the read.
    assert!(ack["promise_id"].is_u64(), "{ack}");
    assert_eq!(ack["visible"], false, "no shape carries a budget: {ack}");
    assert!(ack.get("meters").is_none(), "{ack}");
    let set = c
        .request_ok(&format!("budgets/{}/get", "allaxes"), json!({}))
        .await;
    // All four meters exist and report their own unit.
    assert_eq!(set["meters"]["session"]["unit"], "percent");
    assert_eq!(set["meters"]["weekly_fable"]["limit_pct"], 1.0);
    assert_eq!(set["meters"]["cash"]["unit"], "dollars");

    let mut s = settings(tmp.path());
    s["budget_key"] = json!("allaxes");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;

    let tripped = c.wait_event("budget_tripped").await;
    assert_eq!(tripped["event"]["axis"], "weekly_fable");
    c.wait_event("turn_ended").await;

    let b = c.request_ok(&format!("budgets/{}/get", "allaxes"), json!({})).await;
    assert_eq!(b["weekly_fable"]["status"], "exhausted");
    assert!(b["weekly_fable"]["consumed_tokens"].as_u64().unwrap() > 0);
    // The two account-wide percentage meters saw the same usage and are fine.
    assert_eq!(b["session"]["status"], "calibrating");
    assert_eq!(b["cash"]["status"], "ok");
}

#[tokio::test]
async fn the_crate_refuses_to_default_a_budget_to_all_accounts() {
    // ROUND-7 item 6: "we should probably indicate which accounts they apply
    // to ... I will not want to default to distributing across all of the
    // available accounts because I'm going to have different accounts for
    // different businesses." AUI may choose all-accounts; the crate may not.
    let (_m, addr, _tmp) = start().await;
    let mut c = Client::connect(addr).await;

    let outcome = c
        .request(&format!("budgets/{}/set", "unscoped"), json!({"session_pct": 20.0}))
        .await;
    assert_eq!(outcome["type"], "error");
    assert!(outcome["error"]
        .as_str()
        .unwrap()
        .contains("must name the accounts"));

    // Naming them explicitly works, and the scope round-trips — through the
    // READ, since the write answers an acknowledgement (rule 2.4).
    c.request_ok(
        &format!("budgets/{}/set", "unscoped"),
        json!({"session_pct": 20.0, "accounts": ["aa-owned"]}),
    )
    .await;
    let ok = c
        .request_ok(&format!("budgets/{}/get", "unscoped"), json!({}))
        .await;
    assert_eq!(ok["config"]["accounts"], json!(["aa-owned"]));

    // A scope naming no REGISTERED account parks rather than silently widening.
    c.request_ok(
        &format!("budgets/{}/set", "otherbiz"),
        json!({"session_pct": 20.0, "accounts": ["business-b"]}),
    )
    .await;
    let b = c.request_ok(&format!("budgets/{}/get", "otherbiz"), json!({})).await;
    assert_eq!(b["metered_accounts"], json!([]));
}

#[tokio::test]
async fn a_thread_with_history_fails_over_and_takes_its_conversation_with_it() {
    // ROUND-7 item 7 / Q-33 is SUPERSEDED (msg #1028, intent item 39): "on the
    // thread never moves… that's not what I mean. If we're running a thread on
    // one account and then it hits the budget for that account, we should
    // continue running the thread on the other account."
    //
    // The old rule parked here, protecting the conversation. The conversation
    // is a FILE — `<config_dir>/projects/<cwd-slug>/<session>.jsonl` — so it is
    // ported and the thread continues instead. What has NOT changed: parking is
    // still the answer when nobody admits the thread (that half is asserted in
    // `tests/transcript_port.rs`), and a PIN still never moves.
    let (m, addr, tmp) = start_with_accounts(&["second"]).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;
    // The POOL: two accounts. This asserted three labels *"the machine default
    // among them (CON-27 — it stays listable because the operator manages it)"*
    // until 2026-08-27, when the operator struck that split — *"I do not want
    // default even in the purview of the system"*. The visible list and the
    // placeable list are now the same list.
    assert_eq!(
        m.account_pool(),
        vec!["aa-owned".to_string(), "second".into()]
    );
    assert_eq!(
        m.placement_pool()
            .iter()
            .map(|p| p.name().to_string())
            .collect::<Vec<_>>(),
        vec!["aa-owned".to_string(), "second".into()]
    );

    let p = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // One real turn: the thread now has conversation history on `default`.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    c.wait_event("turn_ended").await;
    let ft = c
        .request_ok(&format!("threads/{}/budget_get", id), json!({}))
        .await;
    assert_eq!(ft["history_accounts"], json!(["aa-owned"]));

    // A real transcript in `default`'s store — the thing the old rule was
    // protecting, and the thing this test now watches move.
    // CON-27: the source store is `aa-owned`'s, not the machine default's —
    // `<config_dir>/projects/<slug>`, where the thread actually lives now.
    let slug = format!("-tmp-e2e-failover-{id}");
    let source_dir = tmp.path().join("aa-owned/projects").join(&slug);
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join(format!("{id}.jsonl"));
    let body: String = (0..5)
        .map(|i| json!({"sessionId": id, "seq": i}).to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&source, &body).unwrap();

    // Forget the first turn's events so the assertions below are about the
    // SECOND delivery only.
    c.drain(Duration::from_millis(150)).await;
    c.bus.as_mut().expect("subscribed").clear();

    // Its account goes over a threshold; the OTHER account has plenty of room.
    c.request_ok(
        &format!("threads/{}/update", id),
        json!({"patch": {"thresholds": {"session": 50.0}}}),
    )
    .await;
    for (account, pct) in [("aa-owned", 90.0), ("second", 5.0)] {
        m.budgets
            .observe(account, threads::gauge::Axis::Session, pct, None, "usage_poll")
            .unwrap();
    }

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "and again"}))
        .await;
    let switched = c.wait_event("account_switched").await;
    assert_eq!(switched["event"]["from"], "aa-owned");
    assert_eq!(switched["event"]["to"], "second");
    c.wait_event("turn_ended").await;

    // The conversation went with it, byte for byte, and the source survives.
    let dest = tmp
        .path()
        .join("second/projects")
        .join(&slug)
        .join(format!("{id}.jsonl"));
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), body);
    assert!(source.is_file(), "a port never deletes the source");

    // It ran rather than parking, and the durable record moved with it.
    assert!(!c.seen("turn_parked"), "an admissible sibling means no park");
    assert_eq!(
        m.store.get_thread(&id).unwrap().unwrap().account.as_deref(),
        Some("second")
    );
    let ft = c
        .request_ok(&format!("threads/{}/budget_get", id), json!({}))
        .await;
    let mut history: Vec<String> = ft["history_accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    history.sort();
    assert_eq!(
        history,
        vec!["aa-owned".to_string(), "second".to_string()],
        "the port records the destination as history, so the continuity gate \
         now treats the thread as living there too"
    );
}

#[tokio::test]
async fn a_fresh_thread_still_starts_on_any_admissible_account() {
    // The other half of item 7: with no conversation to lose, a thread starts
    // wherever there is room.
    //
    // It now starts there at CREATE time rather than switching on its first
    // send: `resolve_account`'s `Auto` arm PLACES the thread instead of taking
    // the alphabetically first account, so the readings below are consulted
    // before the thread row is ever written. Same guarantee, one hop earlier —
    // and it is the hop that matters, because a thread's account is sticky.
    let (m, addr, tmp) = start_with_accounts(&["second"]).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    for (account, pct) in [("aa-owned", 90.0), ("second", 5.0)] {
        m.budgets
            .observe(account, threads::gauge::Axis::Session, pct, None, "usage_poll")
            .unwrap();
    }
    let mut s = settings(tmp.path());
    s["thresholds"] = json!({"session": 50.0});
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // Placed on the emptier account, durably, before a single turn has run.
    assert_eq!(
        m.store.get_thread(&id).unwrap().unwrap().account.as_deref(),
        Some("second"),
        "create-time Auto is a placement, not an alphabetical pick"
    );
    assert_eq!(m.serving_account(&id).unwrap(), "second");

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    c.wait_event("turn_started").await;
    c.wait_event("turn_ended").await;
    assert!(!c.seen("turn_parked"), "a fresh thread has nothing to lose");
    // Nothing to correct at admission — it was already in the right place, so
    // the gate had no reason to move it.
    assert!(
        !c.seen("account_switched"),
        "a correctly placed thread does not need a switch"
    );
}

#[tokio::test]
async fn an_account_switch_persists_the_thread_account_column() {
    // IS-A. `switch_account` used to move only `ThreadState`, so
    // `threads.account` kept its CREATE-time value for the life of the thread
    // and every durable consumer of "which account is this thread on" read a
    // stale answer from the instant the thread failed over. The activity
    // roll-up had to work around it by ranking `thread_accounts` history above
    // the column; with the write in place the column is the durable answer.
    let (m, addr, tmp) = start_with_accounts(&["second"]).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    // Nothing is observed YET, so create-time placement has no measured axis to
    // rank on: both accounts are unknown, both hold zero threads, and the tie
    // falls through to the cursor and the name. The thread starts on `default`,
    // which is what makes the switch below a real switch.
    let mut s = settings(tmp.path());
    s["thresholds"] = json!({"session": 50.0});
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    let column = || {
        m.store
            .get_thread(&id)
            .unwrap()
            .unwrap()
            .account
            .unwrap_or_default()
    };
    // At create time the column names the account the thread started on.
    assert_eq!(column(), "aa-owned");

    // NOW the readings arrive: `default` is over the thread's threshold and
    // `second` is clear, so the admission gate moves it on its first send.
    for (account, pct) in [("aa-owned", 90.0), ("second", 5.0)] {
        m.budgets
            .observe(account, threads::gauge::Axis::Session, pct, None, "usage_poll")
            .unwrap();
    }

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    let sw = c.wait_event("account_switched").await;
    assert_eq!(sw["event"]["from"], "aa-owned");
    assert_eq!(sw["event"]["to"], "second");

    // The switch is DURABLE, and it landed BEFORE the audit event went out —
    // anything that reacts to `account_switched` by reading the store sees the
    // new account, not the create-time one.
    assert_eq!(column(), "second");

    // Which means the activity surface attributes the thread from the store
    // alone, with no help from the daemon's in-memory map.
    let rows = threads::accounts::collect_activity(
        &m.store,
        &m.pool(),
        &std::collections::BTreeMap::new(),
        None,
        100,
    )
    .unwrap();
    let on = |name: &str| rows.iter().find(|r| r.account == name).unwrap().threads;
    assert_eq!((on("aa-owned"), on("second")), (0, 1));

    c.wait_event("turn_started").await;
    c.wait_event("turn_ended").await;
    // The turn ran on `second`, so the history table now agrees with the column
    // rather than contradicting it — and the column still holds after the
    // turn's own bookkeeping has written the row.
    assert_eq!(column(), "second");
    let ft = c
        .request_ok(&format!("threads/{}/budget_get", id), json!({}))
        .await;
    assert_eq!(ft["history_accounts"], json!(["second"]));
}

// ─────────────────────────────────────────────────────────────────────────────
// Q-34 (msg #751): a budget key is DISABLED, never deleted.
//
// "I don't think we should delete a budget key. I think we should disable a
// budget key so we can still query and see information about the budget, like
// how much of the budget did it spend, etc. So no deleting." … and on what a
// disabled key does: "whenever the budget key is used, we just deny."
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_disabled_budget_key_denies_the_turn_stays_queryable_and_enabling_releases_it() {
    // All three halves of the operator's answer in one run: DENY (and PARK, not
    // drop), stay fully QUERYABLE while disabled, and release the parked work
    // the moment the key is enabled again.
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    // A key with plenty of headroom on every axis: nothing but the disable
    // switch can possibly stop this thread.
    calibrate(&m, "aa-owned", "session", 100_000.0);
    c.request_ok(
        &format!("budgets/{}/set", "retired"),
        json!({"session_pct": 90.0, "cash_usd": 100.0,
               "accounts": ["aa-owned"]}),
    )
    .await;

    let mut s = settings(tmp.path());
    s["budget_key"] = json!("retired");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // Spend already on the key, so there is real history to keep querying.
    m.budgets
        .charge("retired", &id, "aa-owned", "claude-sonnet-5", 1_000_000, 2.50)
        .unwrap();

    let off = c.request_ok(&format!("budgets/{}/disable", "retired"), json!({})).await;
    assert_eq!(off["disabled"], true);
    assert!(off["disabled_at"].is_string());
    // Disabling reports the same live state as any other read — it is not a
    // teardown, so the meters come back with it.
    assert_eq!(off["session"]["consumed_tokens"], 1_000_000);

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "please do the work"}),
    )
    .await;

    // DENIED — and parked, with the disabled key named (CON-4).
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "disabled");
    assert_eq!(parked["event"]["budget_key"], "retired");
    let reason = parked["event"]["reason"].as_str().unwrap();
    assert!(reason.contains("retired"), "{reason}");
    assert!(reason.contains("disabled"), "{reason}");
    assert_eq!(parked["event"]["message_ids"], json!([1]));

    // Nothing ran, and the prompt is still in the queue.
    c.drain(Duration::from_millis(400)).await;
    assert!(!c.seen("turn_started"), "a disabled key must deny the turn");
    assert!(
        !c.seen("budgets_failed"),
        "a disable is a GATE, not a module failure"
    );
    let q = queue_rows(addr, &id, "waiting").await;
    assert_eq!(q[0]["message_id"], 1);
    assert_eq!(q[0]["text"], "please do the work");

    // …and EVERYTHING about the key is still queryable while it is disabled:
    // config, spend on every axis, the per-account breakdown, calibration
    // state, and who points at it.
    let b = c.request_ok(&format!("budgets/{}/get", "retired"), json!({})).await;
    assert_eq!(b["disabled"], true);
    assert_eq!(b["config"]["disabled"], true);
    assert!(b["config"]["disabled_at"].is_string());
    assert_eq!(b["config"]["session_pct"], 90.0);
    assert_eq!(b["config"]["accounts"], json!(["aa-owned"]));
    assert_eq!(b["session"]["consumed_tokens"], 1_000_000);
    assert_eq!(b["session"]["consumed_pct"], 10.0);
    assert_eq!(b["session"]["status"], "ok");
    assert_eq!(b["session"]["accounts"][0]["account"], "aa-owned");
    assert_eq!(
        b["session"]["accounts"][0]["calibration_samples"],
        threads::gauge::PRIOR_SAMPLES as u64
    );
    assert_eq!(b["cash"]["consumed"], 2.50);
    assert_eq!(b["threads"], json!([id]));
    assert!(b["estimation"].is_object() || b["estimation"].is_array());
    // No fabricated trip: it is denied by the switch, not by an exhausted meter.
    assert_eq!(b["tripped"], false);
    // It is still listed like any other key.
    let list = budgets_list(&m);
    assert_eq!(list["budgets"][0]["key"], "retired");
    assert_eq!(list["budgets"][0]["disabled"], true);

    // Enabling releases the parked work by itself.
    let on = c.request_ok(&format!("budgets/{}/enable", "retired"), json!({})).await;
    assert_eq!(on["disabled"], false);
    assert_eq!(on["disabled_at"], Value::Null);
    let resumed = c.wait_event("turn_resumed").await;
    assert!(resumed["event"]["reason"]
        .as_str()
        .unwrap()
        .contains("disabled"));
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
    c.wait_event("turn_ended").await;
}

#[tokio::test]
async fn disabling_a_key_mid_turn_lets_it_finish_and_still_records_the_spend() {
    // Disabling is a gate on STARTING, not a demolition of work in flight: the
    // running turn finishes (nothing is torn down, matching default-FINISH),
    // its spend still lands on the ledger — the accounting the operator wants
    // to keep querying must not be missing a turn — and the NEXT turn is denied.
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    c.request_ok(
        &format!("budgets/{}/set", "midflight"),
        json!({"cash_usd": 100.0, "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("midflight");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    c.wait_event("turn_started").await;
    // …and the operator pulls the key out from under it.
    c.request_ok(&format!("budgets/{}/disable", "midflight"), json!({}))
        .await;

    let ended = c.wait_event("turn_ended").await;
    assert_eq!(ended["event"]["exit_code"], 0);
    assert!(
        !c.seen("turn_killed"),
        "disabling a key must not tear down work in flight"
    );

    // The fixture's turn costs $0.01 — it is on the ledger, disabled or not.
    let b = c.request_ok(&format!("budgets/{}/get", "midflight"), json!({})).await;
    assert_eq!(b["disabled"], true);
    assert!(
        b["cash"]["consumed"].as_f64().unwrap() > 0.0,
        "the in-flight turn's spend must still be recorded: {b:#}"
    );

    // The next turn is denied, and waits.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "again"}))
        .await;
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "disabled");
    let q = queue_rows(addr, &id, "waiting").await;
    assert_eq!(q[0]["text"], "again");

    c.request_ok(&format!("budgets/{}/enable", "midflight"), json!({}))
        .await;
    c.wait_event("turn_resumed").await;
    c.wait_event("turn_started").await;
    c.wait_event("turn_ended").await;
}

#[tokio::test]
async fn a_thread_on_a_key_with_no_config_parks_instead_of_running_unmetered() {
    // The other half of "no deleting": the hole `budgets.delete` opened was a
    // thread left pointed at a key that no longer existed and therefore ran
    // UNMETERED and UNGATED. With deletion gone, the last way to reach that
    // state is a key that was never minted — and it parks.
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let mut s = settings(tmp.path());
    s["budget_key"] = json!("ghost");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "unmetered?"}))
        .await;
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "unknown_key");
    assert_eq!(parked["event"]["budget_key"], "ghost");
    assert!(parked["event"]["reason"]
        .as_str()
        .unwrap()
        .contains("budgets/<key>/set"));
    c.drain(Duration::from_millis(400)).await;
    assert!(!c.seen("turn_started"), "nothing runs on an unmeterable key");
    let q = queue_rows(addr, &id, "waiting").await;
    assert_eq!(q[0]["text"], "unmetered?");

    // Pointing a thread at a key that does not exist is refused where the
    // caller can still fix it, rather than only showing up as a park later.
    let refused = c
        .request(&format!("threads/{}/budget_assign", id), json!({"key": "phantom"}))
        .await;
    assert_eq!(refused["type"], "error", "{refused}");
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .contains("no such budget key"));

    // Minting the key is the remedy, and the parked prompt goes out by itself.
    c.request_ok(
        &format!("budgets/{}/set", "ghost"),
        json!({"session_pct": 90.0, "accounts": ["aa-owned"]}),
    )
    .await;
    c.wait_event("turn_resumed").await;
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
    c.wait_event("turn_ended").await;
}

#[tokio::test]
async fn disabling_a_key_outranks_a_latched_kill_and_enabling_does_not_release_it() {
    // The two "this thread is held" mechanisms have to compose in one specific
    // order. A budget KILL latches until the operator moves the limit; a
    // DISABLED key denies for a reason that is both louder and more actionable,
    // so it is the one reported. And enabling the key must NOT quietly release a
    // latched kill — the limit that killed the turn is still the limit.
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    c.request_ok(
        &format!("budgets/{}/set", "latchoff"),
        json!({"cash_usd": 0.01, "on_trip": "kill",
               "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("latchoff");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "killcheap: overshoot then flush a cheap result"}),
    )
    .await;
    c.wait_event("turn_killed").await;
    c.wait_event("turn_ended").await;
    assert_eq!(c.wait_event("turn_parked").await["event"]["cause"], "budget");

    // Disabled now: the park reason changes to the one the operator can act on.
    c.request_ok(&format!("budgets/{}/disable", "latchoff"), json!({}))
        .await;
    let parked = c
        .wait_push(|f| {
            f["type"] == "event"
                && f["event"]["kind"] == "turn_parked"
                && f["event"]["cause"] == "disabled"
        })
        .await;
    assert_eq!(parked["event"]["budget_key"], "latchoff");
    assert!(m.store.get_parked_turn(&id).unwrap().is_some());

    // Enabling puts the LATCH back in charge — it does not release the kill.
    c.request_ok(&format!("budgets/{}/enable", "latchoff"), json!({}))
        .await;
    let relatched = c
        .wait_push(|f| {
            f["type"] == "event"
                && f["event"]["kind"] == "turn_parked"
                && f["event"]["cause"] == "budget"
        })
        .await;
    assert!(relatched["event"]["reason"]
        .as_str()
        .unwrap()
        .contains("killed this turn"));
    c.drain(Duration::from_millis(1000)).await;
    assert!(
        !c.seen("turn_resumed"),
        "enabling a key must not release a latched kill"
    );
    assert!(m.store.get_parked_turn(&id).unwrap().is_some());

    // Moving the limit is still the one escape hatch (DEC-11), and it works.
    c.request_ok(&format!("budgets/{}/set", "latchoff"), json!({"cash_usd": 500.0}))
        .await;
    c.wait_event("turn_resumed").await;
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
}

// ─────────────────────────────────────────────────────────────────────────────
// The 2026-07-30 conformance audit's two real holes (findings F1, F2) and the
// proof decision 8 was missing (F7).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_retry_goes_through_the_admission_gate_and_parks_when_it_is_refused() {
    // AUDIT F1. `fire_retry` used to call `run_turn` DIRECTLY: `admit_or_park`
    // was reachable from exactly two sites — parked-turn redelivery and the
    // queue dequeue — and a retry was on neither. So a retry armed before the
    // gate closed ran anyway: a disabled key, an exhausted FINISH meter, a
    // tripped threshold and a budgets-module fault were all invisible to it,
    // and fail-CLOSED did not cover the path at all.
    //
    // This is also the compounding case behind F7: the 429 that arms a
    // rate-limit retry is the same event that records the 100% observation the
    // gate would refuse on, and the retry it armed can be hours out.
    let (m, addr, tmp) = start_without_polling().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let mut s = settings(tmp.path());
    s["thresholds"] = json!({"fable": 60.0});
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // Nothing observed yet, so the gate has nothing to refuse: the first turn
    // runs, dies transiently, and arms a retry ~2s out.
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": format!("failonce {id}")}),
    )
    .await;
    c.wait_event("turn_started").await;
    assert_eq!(c.wait_event("turn_ended").await["event"]["exit_code"], 1);
    let rs = c.wait_event("retry_scheduled").await;
    assert_eq!(rs["event"]["error_kind"], "transient");
    assert_eq!(rs["event"]["attempt"], 2);

    // The gate closes while the timer is armed.
    m.budgets
        .observe(
            "aa-owned",
            threads::gauge::Axis::WeeklyFable,
            88.0,
            None,
            "usage_poll",
        )
        .unwrap();

    // The retry fires into the GATE, not into a spawn.
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "threshold");
    assert!(parked["event"]["reason"]
        .as_str()
        .unwrap()
        .contains("weekly_fable"));
    assert_eq!(parked["event"]["message_ids"], json!([1]));

    c.drain(Duration::from_millis(600)).await;
    assert_eq!(
        ledger_count(&mut c, &id, "turn_started").await,
        1,
        "a refused retry must not spawn a second execution"
    );

    // CON-4, on the retry path too: the prompt moved out of the retry row into
    // the durable parked row — not duplicated across both, not dropped.
    //
    // THE HAND-OVER IS THE SECOND REFUSAL, AND WHICH ONE IT IS MOVED.
    //
    // The `observe` above rings `account_picture_changed`, so the first refusal
    // arrives on a QUESTION-origin pass. A question's refusal is information: it
    // emits `turn_parked` — the event asserted above — and deliberately leaves the
    // prompt in `retries` so the next reconsideration can ask again.
    //
    // The hand-over belongs to the POOL'S CADENCE, which is the only origin that is
    // a deadline (it used to belong to an elapsed per-thread `retry_at`; the
    // operator removed that timestamp). This row is `transient` at attempt 2, so the
    // pool's rung is `transient_backoff_secs(2)` = 5 seconds, and the cadence pass
    // that fires then takes the lock, is refused again, and hands the prompt over.
    //
    // Wait for it rather than for a fixed drain; the `turn_started` count above is
    // what proves nothing ran in the meantime. **This loop is also the regression
    // guard on cadence starvation**: re-arming the pool's deadline on every pass
    // rather than only on the one that consumed it lets the 5-second backstop push
    // it out forever, and this is the assertion that caught that.
    for _ in 0..200 {
        if m.store.get_retry(&id).unwrap().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "the retry row was handed over to `parked_turns`"
    );
    let row = m
        .store
        .get_parked_turn(&id)
        .unwrap()
        .expect("the refused retry's prompt is parked, verbatim");
    assert!(row.prompt.contains("failonce"));
    assert_eq!(row.message_ids, vec![1]);
    assert_eq!(row.attempt, 2, "the attempt number survives the park");

    // Usage clears and the parked retry goes out by itself — ONCE, still as
    // attempt 2, so the retry semantics are intact.
    m.budgets
        .observe(
            "aa-owned",
            threads::gauge::Axis::WeeklyFable,
            9.0,
            None,
            "usage_poll",
        )
        .unwrap();
    c.wait_event("turn_resumed").await;
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
    assert_eq!(ts["event"]["attempt"], 2);
    assert_eq!(c.wait_event("turn_ended").await["event"]["exit_code"], 0);

    c.drain(Duration::from_millis(400)).await;
    assert_eq!(
        ledger_count(&mut c, &id, "turn_started").await,
        2,
        "the parked retry is delivered exactly once"
    );
    assert!(m.store.get_parked_turn(&id).unwrap().is_none());
}

#[tokio::test]
async fn an_unreadable_gauge_store_fails_the_threshold_gate_closed() {
    // AUDIT F2, the one genuine fail-OPEN seam that was left. The threshold
    // loop asks `observed_value`, which ended in `.ok()` — so a SQLite fault
    // was indistinguishable from "the gauge has never reported this axis", and
    // an unobserved axis means NEVER GATE (CON-5). A broken database therefore
    // ADMITTED, which is precisely what round-7 item 1 forbids.
    //
    // The distinction the fix restores: a genuine ABSENCE still admits (that is
    // CON-5 and it is right); an unreadable STORE fails closed.
    let (m, addr, tmp) = start_without_polling().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let mut s = settings(tmp.path());
    s["thresholds"] = json!({"session": 50.0});
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // Break the gauge's own table underneath the gate.
    m.store.execute_sql("DROP TABLE usage_observations").unwrap();

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "spend my money"}),
    )
    .await;

    // LOUD, and it names the real fault instead of inventing an empty gauge.
    let failed = c.wait_event("budgets_failed").await;
    assert_eq!(failed["event"]["phase"], "admission");
    assert_eq!(failed["event"]["outcome"], "parked");
    assert!(
        failed["event"]["error"]
            .as_str()
            .unwrap()
            .contains("usage_observations"),
        "{failed}"
    );

    // CLOSED: it does not run…
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "internal");
    assert!(parked["event"]["reason"]
        .as_str()
        .unwrap()
        .contains("budgeting_off"));
    c.drain(Duration::from_millis(500)).await;
    assert!(
        !c.seen("turn_started"),
        "an unreadable gauge must not admit the turn"
    );

    // …and CON-4 still holds: the work is parked, not dropped.
    let q = queue_rows(addr, &id, "waiting").await;
    assert_eq!(q[0]["text"], "spend my money");

    // `budgeting_off` remains the only way through, exactly as for every other
    // internal failure.
    c.request_ok(
        &format!("threads/{}/update", id),
        json!({"patch": {"budgeting_off": true}}),
    )
    .await;
    c.wait_event("turn_resumed").await;
    let ts = c.wait_event("turn_started").await;
    assert_eq!(ts["event"]["message_ids"], json!([1]));
    c.wait_event("turn_ended").await;
}

#[tokio::test]
async fn a_rate_limit_observation_gates_the_next_turn() {
    // AUDIT F7 / decision 8: the 429 backstop is wired into the ADMISSION path,
    // not merely parsed. Every other end-to-end gating test feeds the gauge
    // from a `usage_poll`; this daemon never polls at all, so the only reading
    // the gate can possibly have used is the one claude's own
    // `rate_limit_event` stream line recorded.
    let (_m, addr, tmp) = start_without_polling().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let mut s = settings(tmp.path());
    s["thresholds"] = json!({"weekly": 60.0});
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    // Nothing observed ⇒ nothing to gate on. The turn runs — and it is the turn
    // itself that reports the account is 73% through its week.
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "gaugeturn please"}),
    )
    .await;
    c.wait_event("rate_limit_event").await;
    c.wait_event("turn_ended").await;

    let g = c.request_ok("gauge/status", json!({})).await;
    let weekly = &g["accounts"][0]["axes"]["weekly_all"];
    assert_eq!(weekly["observed_pct"], 73.0);
    assert_eq!(
        weekly["source"], "rate_limit_event",
        "the gate must be reading a 429-class observation, not a poll"
    );

    // The NEXT turn is the one that observation refuses.
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "and again"}),
    )
    .await;
    let parked = c.wait_event("turn_parked").await;
    assert_eq!(parked["event"]["cause"], "threshold");
    let reason = parked["event"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("weekly_all") && reason.contains("73"),
        "{reason}"
    );
    // #1 was the prompt, #2 the assistant's reply, so the refused send is #3.
    assert_eq!(parked["event"]["message_ids"], json!([3]));

    // Parked, not dropped, and no second execution.
    c.drain(Duration::from_millis(500)).await;
    assert_eq!(ledger_count(&mut c, &id, "turn_started").await, 1);
    let q = queue_rows(addr, &id, "waiting").await;
    assert_eq!(q[0]["text"], "and again");
}

#[tokio::test]
async fn a_429_limit_hit_gates_every_thread_on_that_account() {
    // AUDIT F7, the other source. A synthetic 429 classifies its pool and
    // records a 100% observation with source `limit_hit`; thresholds gate over
    // the account POOL (DEC-6/DEC-10), so that reading holds back every thread
    // sitting on the account — not just the one that hit the limit.
    let (_m, addr, tmp) = start_without_polling().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    // The thread that takes the 429. No thresholds: it must be admitted.
    let hit = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let hit_id = hit["thread_id"].as_str().unwrap().to_string();
    c.request_ok(
        &format!("threads/{}/send", hit_id),
        json!({"text": "ratelimit me"}),
    )
    .await;
    let lh = c.wait_event("limit_hit").await;
    assert_eq!(lh["event"]["status"], 429);
    assert_eq!(lh["event"]["pool"], "session");
    c.wait_event("retry_scheduled").await;

    let g = c.request_ok("gauge/status", json!({})).await;
    let session = &g["accounts"][0]["axes"]["session"];
    assert_eq!(session["observed_pct"], 100.0);
    assert_eq!(session["source"], "limit_hit");

    // A DIFFERENT thread, gated on the session axis, is now refused by it.
    let mut s = settings(tmp.path());
    s["thresholds"] = json!({"session": 50.0});
    let other = c.request_ok("threads/create", json!({"settings": s})).await;
    let other_id = other["thread_id"].as_str().unwrap().to_string();
    c.request_ok(
        &format!("threads/{}/send", other_id),
        json!({"text": "work please"}),
    )
    .await;

    let parked = c
        .wait_push(|f| {
            f["type"] == "event"
                && f["event"]["kind"] == "turn_parked"
                && f["thread_id"] == other_id.as_str()
        })
        .await;
    assert_eq!(parked["event"]["cause"], "threshold");
    assert!(parked["event"]["reason"]
        .as_str()
        .unwrap()
        .contains("session at 100"));
    c.drain(Duration::from_millis(500)).await;
    assert_eq!(ledger_count(&mut c, &other_id, "turn_started").await, 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// The SUBAGENT DOUBLE-COUNT (readiness audit, 2026-07-31).
//
// `modelUsage` on a `result` line is the CUMULATIVE per-model total for the
// whole claude process, and a turn with subagents emits one result per
// subagent. The pre-fix daemon recorded each snapshot verbatim and reconciled
// the meter against it, so one live turn was booked seventeen times over
// ($2,845 on a thread whose true spend was about $199) and every budget — cash
// included — would have tripped at roughly a tenth of its stated amount.
//
// The `multiresult` fixture reproduces that shape exactly: three results at one
// running total, then fourteen at a grown one. Truth is 4,800 tokens and $4.00;
// the pre-fix arithmetic reads 70,800 tokens and $59.00.
// ─────────────────────────────────────────────────────────────────────────────

/// One turn, seventeen results: the ledger sums to what the API billed, and the
/// meter settles on the same number — once.
#[tokio::test]
async fn a_turn_with_many_results_is_recorded_once_at_its_true_cost() {
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    // Deliberately far above the truth AND above the inflated figure, so this
    // test measures the NUMBER rather than enforcement.
    c.request_ok(
        &format!("budgets/{}/set", "multi"),
        json!({"cash_usd": 1000.0, "accounts": ["aa-owned"]}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("multi");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "multiresult please"}),
    )
    .await;
    c.wait_event("turn_ended").await;

    // Seventeen results really did arrive — the fixture is exercising the shape
    // this test is about, not a degenerate single-result turn.
    assert_eq!(
        ledger_count(&mut c, &id, "result_turn").await,
        17,
        "the fixture must emit the seventeen-result shape"
    );

    // THE LEDGER. $4.00 once, not $59.00; one turn, not seventeen rows called
    // turns.
    // Read off the store in-process: `ledger/query`, the wire read that used to
    // answer this, had no caller and was RETIRED at R2.S3a. `spend_summary` is
    // the function that arm called, and the budget calibration still calls it.
    let led = _m.store.spend_summary(false).unwrap();
    let row = led
        .iter()
        .find(|r| r.key == id)
        .expect("the thread has a ledger row");
    let cost = row.cost_usd;
    assert!(
        (cost - 4.0).abs() < 1e-9,
        "the ledger must sum to the $4.00 the turn actually cost, got ${cost}: {row:?}"
    );
    assert_eq!(row.total_tokens, 4_800, "row: {row:?}");
    assert_eq!(row.turns, 1, "one execution, not seventeen rows: {row:?}");
    // A fresh store has nothing from the old writer to warn about.
    assert_eq!(led.iter().map(|r| r.legacy_rows).sum::<u64>(), 0, "{led:?}");

    // THE METER, settled exactly once against the same number.
    let b = c.request_ok(&format!("budgets/{}/get", "multi"), json!({})).await;
    let cash = b["cash"]["consumed_usd"].as_f64().unwrap();
    assert!(
        (cash - 4.0).abs() < 1e-9,
        "the cash meter must settle on $4.00, got ${cash}: {b:#}"
    );
    assert_eq!(
        b["session"]["consumed_tokens"], 4_800,
        "and the token meter on 4,800: {b:#}"
    );
}

/// The operator-facing consequence, proved: a $10 budget does not trip on a
/// $4.00 turn.
///
/// Before the fix the same turn metered $59.00, so the budget tripped at 40% of
/// its stated amount and parked the operator's next prompt. This is why the
/// readiness report had to say "do not turn on any budget yet, cash included".
#[tokio::test]
async fn a_budget_no_longer_trips_at_a_fraction_of_its_stated_amount() {
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    // $10 sits comfortably above the turn's true $4.00 and comfortably below
    // the $59.00 the double-count read.
    c.request_ok(
        &format!("budgets/{}/set", "tenth"),
        json!({"cash_usd": 10.0, "accounts": ["aa-owned"], "on_trip": "kill"}),
    )
    .await;
    let mut s = settings(tmp.path());
    s["budget_key"] = json!("tenth");
    let p = c.request_ok("threads/create", json!({"settings": s})).await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": "multiresult please"}),
    )
    .await;
    c.wait_event("turn_ended").await;
    c.drain(Duration::from_millis(300)).await;

    assert!(
        !c.seen("budget_tripped"),
        "a $4.00 turn must not trip a $10 budget"
    );
    let b = c.request_ok(&format!("budgets/{}/get", "tenth"), json!({})).await;
    assert_eq!(b["cash"]["status"], "ok", "{b:#}");
    let cash = b["cash"]["consumed_usd"].as_f64().unwrap();
    assert!((cash - 4.0).abs() < 1e-9, "meter reads ${cash}: {b:#}");

    // …and the operator's NEXT prompt still runs, rather than being parked
    // against a budget that was never really spent.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    c.wait_push(|f| {
        f["type"] == "event" && f["event"]["kind"] == "turn_started" && f["thread_id"] == id.as_str()
    })
    .await;
    assert!(!c.seen("turn_parked"), "the next turn must not be parked");
}

// ── INTENT 72b: every thread is associated with a budget ────────────────────
//
// "Let's update threads to like force a budget on each consumer. So every
// single thread has to be associated with the budget."
//
// Forced by INHERITANCE, never by REFUSAL — and shipped with the key carrying
// no ceiling on any axis, so the first change alters ACCOUNTING and not
// BEHAVIOUR. The 60/60/60 stops are a separate, independently revertible step.

#[tokio::test]
async fn every_thread_is_associated_with_a_budget_without_naming_one() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    // A client that has never heard of budgets.
    let p = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    assert_eq!(
        m.thread_budget_key(&id).unwrap().as_deref(),
        Some(threads::budgets::DEFAULT_BUDGET_KEY),
        "a thread that named no budget must be GIVEN one"
    );
}

#[tokio::test]
async fn a_thread_is_never_refused_for_want_of_a_budget() {
    // The audio-interface constraint, pinned as behaviour. A thread is created
    // when he says "new thread" with no hands free; if creation could fail for
    // a configuration reason, a repair dialogue would stand between him and the
    // sentence he actually wanted to say. There is no acceptable spoken form of
    // "I could not start your thread because it has no budget."
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    // No budget exists on this daemon at all, and creation still succeeds.
    assert!(
        budgets_list(&_m)["budgets"]
            .as_array()
            .is_none_or(|b| b.iter().all(|x| x["key"]
                != threads::budgets::DEFAULT_BUDGET_KEY)),
        "precondition: the default key has not been minted in this fixture"
    );
    let created = c
        .request("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    assert_eq!(created["type"], "success", "creation must never refuse: {created}");
}

#[tokio::test]
async fn the_inherited_default_gates_nothing_and_the_turn_runs() {
    // THE WHOLE POINT OF THIS STEP. Every thread is now keyed, and every thread
    // must still run exactly as it did. If this fails, forcing budgets onto the
    // fleet has changed behaviour rather than accounting.
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let p = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    c.wait_event("turn_started").await;
    c.wait_event("turn_ended").await;
    assert!(
        !c.seen("turn_parked"),
        "a thread holding only the inherited default must never park on it"
    );
}

#[tokio::test]
async fn a_thread_stored_before_this_change_inherits_on_read() {
    // The backfill, and there is no script to it. Every thread written before
    // this change has no `budget_key` in its settings blob, so the serde default
    // supplies one the moment the blob is read — no migration, no daemon
    // restart, nothing run against the live database.
    let legacy = json!({"cwd": "/tmp", "model": "sonnet"}).to_string();
    let s: threads::settings::ThreadSettings = serde_json::from_str(&legacy).unwrap();
    assert_eq!(
        s.budget_key.as_deref(),
        Some(threads::budgets::DEFAULT_BUDGET_KEY)
    );
}

#[tokio::test]
async fn a_deliberate_unkey_is_not_undone_by_the_default() {
    // Absent means INHERIT; an explicit null means UNKEYED. If those two were
    // not distinguishable on disk, the operator's deliberate act would silently
    // reverse itself the next time the blob was loaded.
    let unkeyed = json!({"cwd": "/tmp", "model": "sonnet", "budget_key": null}).to_string();
    let s: threads::settings::ThreadSettings = serde_json::from_str(&unkeyed).unwrap();
    assert!(s.budget_key.is_none(), "an explicit null unkeys");

    let round_tripped: threads::settings::ThreadSettings =
        serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert!(
        round_tripped.budget_key.is_none(),
        "and it must survive being written back out and read again"
    );
}

#[tokio::test]
async fn the_default_budget_is_minted_with_no_ceiling_on_any_axis() {
    let (m, _addr, _tmp) = start().await;
    assert!(m.budgets.ensure_default(&m.account_pool()).unwrap());

    let cfg = m
        .budgets
        .get_config(threads::budgets::DEFAULT_BUDGET_KEY)
        .unwrap()
        .expect("the default key must exist after seeding");
    assert_eq!(cfg.session_pct, None);
    assert_eq!(cfg.weekly_pct, None);
    assert_eq!(cfg.fable_pct, None);
    assert_eq!(cfg.cash_usd, None);
    assert!(!cfg.disabled);
    assert!(
        !cfg.enforces(),
        "the default must be able to METER but never to REFUSE"
    );
    assert!(
        !cfg.accounts.is_empty(),
        "the crate never defaults a scope to all accounts — the caller names them"
    );
}

#[tokio::test]
async fn seeding_the_default_never_overwrites_a_ceiling_he_set() {
    // Boot must not quietly undo a limit. This is what makes turning the stops
    // ON a durable act rather than one that survives until the next restart.
    let (m, _addr, _tmp) = start().await;
    m.budgets.ensure_default(&m.account_pool()).unwrap();

    let mut cfg = m
        .budgets
        .get_config(threads::budgets::DEFAULT_BUDGET_KEY)
        .unwrap()
        .unwrap();
    cfg.session_pct = Some(60.0);
    m.budgets.set_config(&cfg).unwrap();

    assert!(
        !m.budgets.ensure_default(&m.account_pool()).unwrap(),
        "a second boot must be a no-op"
    );
    assert_eq!(
        m.budgets
            .get_config(threads::budgets::DEFAULT_BUDGET_KEY)
            .unwrap()
            .unwrap()
            .session_pct,
        Some(60.0),
        "the ceiling he set must survive the next boot"
    );
}

#[tokio::test]
async fn a_budget_that_cannot_refuse_does_not_switch_the_gauge_pollers_on() {
    // Polling `/usage` costs real requests against his own accounts, so turning
    // it on is a behaviour change. Both pollers used to ask whether any budget
    // EXISTS; with a default key on every thread that is true forever. They ask
    // whether any budget can actually refuse.
    let (m, _addr, _tmp) = start().await;
    m.budgets.ensure_default(&m.account_pool()).unwrap();

    assert!(
        m.budgets.any_configured().unwrap(),
        "the default key exists…"
    );
    assert!(
        !m.budgets.any_enforcing().unwrap(),
        "…and still nothing is enforced, so nothing needs a gauge"
    );

    let mut cfg = m
        .budgets
        .get_config(threads::budgets::DEFAULT_BUDGET_KEY)
        .unwrap()
        .unwrap();
    cfg.session_pct = Some(60.0);
    m.budgets.set_config(&cfg).unwrap();
    assert!(
        m.budgets.any_enforcing().unwrap(),
        "putting a ceiling on it is what turns enforcement — and polling — on"
    );
}

#[tokio::test]
async fn a_key_the_operator_named_that_does_not_exist_still_parks() {
    // The exemption is for the crate's OWN default key and nothing else. A key
    // he typed that does not exist must still refuse, or `budgets.delete`'s hole
    // reopens under a new name.
    let (m, _addr, tmp) = start().await;
    let mut s = threads::settings::ThreadSettings::new(
        tmp.path().to_string_lossy().to_string(),
        "sonnet".to_string(),
    );
    s.budget_key = Some("typo-key".into());
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.thread_budget_key(&id).unwrap().as_deref(), Some("typo-key"));

    // And the default is the only exempt key.
    assert_ne!(threads::budgets::DEFAULT_BUDGET_KEY, "typo-key");
}

#[tokio::test]
async fn the_inherited_default_runs_even_when_its_scope_has_gone_stale() {
    // THE FLEET-STOPPING CASE, and the reason `ungated` had to stop meaning
    // "no key".
    //
    // The default budget is seeded at boot against the accounts registered THEN.
    // Register another account later — or move a thread onto one — and that
    // thread's key is scoped to a set its account is not in. Under the old
    // reading, having a key at all made the thread gated, so the scope miss
    // parked it; and because every thread now has a key, that is every thread
    // on the box, stopped, over a bookkeeping detail on a budget that was never
    // meant to stop anything.
    //
    // The key can't refuse — no ceiling on any axis — so the thread runs.
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let mut cfg = threads::BudgetConfig::new(threads::budgets::DEFAULT_BUDGET_KEY);
    cfg.accounts = vec!["an-account-that-is-not-in-the-pool".into()];
    m.budgets.set_config(&cfg).unwrap();

    let p = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = p["thread_id"].as_str().unwrap().to_string();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    c.wait_event("turn_started").await;
    c.wait_event("turn_ended").await;
    assert!(
        !c.seen("turn_parked"),
        "a budget that cannot refuse must never park a thread, whatever its scope says"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
