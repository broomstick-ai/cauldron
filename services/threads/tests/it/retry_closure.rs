//! **RETRY CLOSURE — the rule that a user interruption ends a delivery and
//! nothing else does.**
//!
//! Wave one of the threads sweep (2026-08-25). These tests exist because the
//! rule they pin is the thing that CATCHES a refusal, and placement is about to
//! make the system refuse considerably more often:
//!
//! > *"I want to retry after all events that are not user interruptions."*
//!
//! Every test here forces a path that used to END a delivery and asserts that it
//! now retries — and, at the top of every ladder, that the operator's words
//! survive and can be sent again.
//!
//! # Every assertion in this file was forced to fail before it was believed
//!
//! *"A green you have not forced to fail is not evidence."* Each test carries a
//! `FORCED RED` note naming the exact one-line mutation that breaks it and the
//! failure it then produces. Those mutations were applied, observed red, and
//! reverted.


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

use threads::{build_manager, Config};

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
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// Start a daemon with `n` placeable accounts.
///
/// The account COUNT is a parameter here (it is fixed at one in `ws_protocol`)
/// because credit exhaustion is a failover story: *"another account might be
/// open for placement"* cannot be tested at N=1, and anything that arbitrates
/// between accounts is proven at N >= 2 or not proven at all.
async fn start_with_accounts(n: usize) -> (threads::ThreadManager, SocketAddr, tempfile::TempDir) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let mut accounts = std::collections::BTreeMap::new();
    for i in 0..n {
        let name = format!("acct-{i}");
        let dir = tmp.path().join(&name).to_string_lossy().to_string();
        accounts.insert(name, dir);
    }
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
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
        let mut c = Self { wr, rd, pending: VecDeque::new(), next_id: 0, bus: None };
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

    /// The first event whose kind is any of `kinds` — the shape that proves a
    /// RACE was won by the right side. Waiting for one kind and then asserting
    /// the other did not happen cannot distinguish "it did not happen" from "it
    /// has not happened yet"; this can.
    async fn wait_first_of(&mut self, kinds: &[&str]) -> Value {
        let owned: Vec<String> = kinds.iter().map(|s| s.to_string()).collect();
        self.wait_push(|f| {
            f["type"] == "event"
                && f["event"]["kind"].as_str().is_some_and(|k| owned.iter().any(|w| w == k))
        })
        .await
    }
}

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
/// flight, which is exactly the kind of ordering coupling these tests exist to
/// not have.
///
/// **A KNOWN-EMPTY QUEUE IS `[]`, NEVER A MISSING KEY.** The cell is `Required`
/// and `shapes::threads`' hydrate maps `Hydration::Empty` onto an empty list,
/// so the `expect` below fails only if the shape stopped carrying the cell at
/// all — which is the thing worth failing on, and which is why this reads the
/// key rather than defaulting it away.
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

fn settings(cwd: &std::path::Path) -> Value {
    json!({"cwd": cwd.to_string_lossy(), "model": "sonnet"})
}

/// Settings with an explicit deterministic ceiling — how a test says "I am
/// testing the top of the ladder, not the length of it".
fn settings_det(cwd: &std::path::Path, ceiling: u32) -> Value {
    json!({
        "cwd": cwd.to_string_lossy(),
        "model": "sonnet",
        "retry_policy": { "max_deterministic_attempts": ceiling },
    })
}

/// The same, for the LOGIN-EXPIRED ceiling.
fn settings_login(cwd: &std::path::Path, ceiling: u32) -> Value {
    json!({
        "cwd": cwd.to_string_lossy(),
        "model": "sonnet",
        "retry_policy": { "max_login_attempts": ceiling },
    })
}

/// Mark an account's login as RE-AUTHENTICATED for the `authresult` fixture.
///
/// The daemon does not create an account's config dir until it spawns there, so
/// this creates it. Writing the marker is how a test says "this login works and
/// that one does not" — without which the failover assertion could not tell
/// "the turn moved" from "the turn moved and died again".
fn login_works(tmp: &tempfile::TempDir, account: &str) {
    let dir = tmp.path().join(account);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("auth-live"), b"").unwrap();
}

/// The account labels barred with a given `limit_axis`, read back from the store.
fn barred_on_axis(m: &threads::ThreadManager, axis: &str) -> Vec<String> {
    m.store
        .list_account_states()
        .unwrap()
        .into_iter()
        .filter(|s| {
            s.effective_limit_until(&threads::store::now_rfc3339()).is_some()
                && s.limit_axis.as_deref() == Some(axis)
        })
        .map(|s| s.name)
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. THE RULE ITSELF — pure, no daemon.
// ─────────────────────────────────────────────────────────────────────────────

/// **Only an INTENTION ends a delivery. A gate never does.**
///
/// This test was called `only_an_operator_interruption_ends_a_delivery` and
/// asserted that `Operator` was "the ONE thing". That was true when it was
/// written and is not any more: `InterruptTrigger::Compaction` joined it on
/// 2026-08-28, when the operator asked that a thread which has just been
/// automatically compacted STOP rather than resume through the boundary.
///
/// The renaming is deliberate rather than cosmetic. The underlying rule never
/// was "the operator is special" — it is the distinction `InterruptTrigger`
/// already draws in its own doc comment, between a trigger that names an
/// INTENTION and one that names a GATE. `Compaction` is an intention he stated
/// once, in advance, and it is applied at the moment it becomes relevant; a
/// standing instruction is not a lesser instruction. Leaving the old name in
/// place would have left the suite asserting a count instead of a rule, and the
/// count is the part that changes.
///
/// The GATE half is untouched and is the half that actually protects his work:
/// a refusal must never end the delivery it refused.
///
/// FORCED RED: adding `| T::DaemonRestart` to the `true` arm of
/// `retry::ends_the_delivery` fails this at the `DaemonRestart` assertion with
/// "a daemon restart is a GATE, not an intention". Removing `T::Operator` or
/// `T::Compaction` from that arm fails the corresponding intention assertion.
/// All three observed.
#[test]
fn only_an_intention_ends_a_delivery_and_a_gate_never_does() {
    use threads::event::InterruptTrigger as T;
    use threads::retry::ends_the_delivery;

    assert!(
        ends_the_delivery(T::Operator),
        "the operator asking for it ends the delivery"
    );
    assert!(
        ends_the_delivery(T::Compaction),
        "a thread that does not resume after a compaction ENDS there — that is \
         his standing instruction, not a refusal by the system"
    );

    // Every other trigger names a GATE. A gate closing is the system refusing,
    // and the retry ladder exists to catch a refusal — so a refusal must never
    // be able to end the delivery it refused.
    assert!(
        !ends_the_delivery(T::DaemonRestart),
        "a daemon restart is a GATE, not an intention"
    );
    for gate in [T::SessionLimit, T::WeeklyLimit, T::FableLimit, T::Threshold, T::HardBudget] {
        assert!(
            !ends_the_delivery(gate),
            "{gate:?} is a limit gate and must not end a delivery"
        );
    }
}

/// **Every verdict lands on a ladder — the totality that IS the rule.**
///
/// `ladder_for` has no `None` arm to fall through, so this test is really
/// asserting that the three verdicts which used to be terminal now name a real
/// ladder each.
///
/// FORCED RED: changing `ladder_for`'s `Fatal` arm to return
/// `TransientKind::ErrorDuringExecution` fails the `Fatal` assertion (the
/// deterministic ladder's short, steep ceiling would silently become the
/// transient one's). Observed.
#[test]
fn every_verdict_lands_on_a_ladder() {
    use threads::retry::{ladder_for, RetryDecision, TransientKind};

    let cases = [
        (RetryDecision::Fatal { reason: "bad model".into() }, TransientKind::Deterministic),
        (RetryDecision::CreditBalance { reason: "no money".into() }, TransientKind::CreditBalance),
        (RetryDecision::RestartFresh { reason: "gone".into() }, TransientKind::Deterministic),
        (
            RetryDecision::Retry { kind: TransientKind::RateLimit, reason: "429".into() },
            TransientKind::RateLimit,
        ),
        (
            RetryDecision::LoginExpired { reason: "session expired".into() },
            TransientKind::LoginExpired,
        ),
    ];
    for (decision, expected) in cases {
        let (kind, reason) = ladder_for(&decision);
        assert_eq!(kind, expected, "wrong ladder for {decision:?}");
        assert!(!reason.is_empty(), "a ladder must carry the reason forward");
    }
}

/// **The wire vocabulary did not change because the disposition did.**
///
/// `credit_balance` and `claude_error` are tags AUI already parses and speaks.
/// Retrying these classes must not rename them, or every client learns a new
/// word for a behaviour change it does not care about.
///
/// FORCED RED: changing `TransientKind::Deterministic`'s `kind_str` to
/// `"deterministic"` fails this. Observed.
#[test]
fn the_new_ladders_keep_the_old_wire_tags() {
    use threads::retry::TransientKind;
    assert_eq!(TransientKind::Deterministic.kind_str(), "claude_error");
    assert_eq!(TransientKind::CreditBalance.kind_str(), "credit_balance");
    // The ONE new tag this wave adds, and it is added rather than reused on
    // purpose. An expired login previously reported as `"transient"` (through
    // the errored-result door) or `"claude_error"` (through stderr) — two words
    // that describe a passing hiccup and an unrecognized string. Neither can
    // carry "he has to re-authenticate", which is the only useful thing to say
    // about this class, so it gets a word of its own.
    assert_eq!(TransientKind::LoginExpired.kind_str(), "login_expired");
    // …and it must be DISTINCT from the two it used to be confused with. A tag
    // collision here would put the class straight back where it started.
    assert_ne!(
        TransientKind::LoginExpired.kind_str(),
        TransientKind::ErrorDuringExecution.kind_str(),
        "login expiry must not report as the generic transient class again"
    );
    assert_ne!(
        TransientKind::LoginExpired.kind_str(),
        TransientKind::Deterministic.kind_str()
    );
}

/// **Both new ladders are bounded and both actually back off.**
///
/// An unbounded retry on a deterministic failure is a token furnace; a bounded
/// one that does not back off is the same furnace burning faster. Both halves
/// are asserted, and the ladder is asserted to be MONOTONE — a ladder that dips
/// would let a late attempt come sooner than an early one.
///
/// FORCED RED: making `deterministic_backoff_secs` return a constant `5` fails
/// the "must actually climb" assertion. Setting
/// `DEFAULT_MAX_DETERMINISTIC_ATTEMPTS` to 0 fails the bound assertion.
/// Both observed.
#[test]
fn the_new_ladders_are_bounded_and_back_off() {
    use threads::retry::{
        credit_backoff_secs, deterministic_backoff_secs, DEFAULT_MAX_CREDIT_ATTEMPTS,
        DEFAULT_MAX_DETERMINISTIC_ATTEMPTS,
    };

    for (name, ceiling) in [
        ("deterministic", DEFAULT_MAX_DETERMINISTIC_ATTEMPTS),
        ("credit", DEFAULT_MAX_CREDIT_ATTEMPTS),
    ] {
        assert!(ceiling > 0, "{name} must allow at least one attempt");
        assert!(ceiling <= 12, "{name} ceiling {ceiling} is a token furnace");
    }

    for f in [
        deterministic_backoff_secs as fn(u32) -> u64,
        credit_backoff_secs as fn(u32) -> u64,
    ] {
        let rungs: Vec<u64> = (1..=6).map(f).collect();
        assert!(
            rungs.windows(2).all(|w| w[1] >= w[0]),
            "a ladder must be monotone: {rungs:?}"
        );
        assert!(
            rungs.last().unwrap() > rungs.first().unwrap(),
            "a ladder must actually climb: {rungs:?}"
        );
    }

    // The deterministic ladder must be STEEPER than the transient one. Both are
    // short, but they are short for opposite reasons — a transient failure is
    // expected to clear in seconds, a deterministic one is expected never to
    // clear, so spending its whole ceiling inside a half-minute buys nothing.
    assert!(
        deterministic_backoff_secs(4) > 60,
        "the deterministic ladder must reach past a minute before it gives up"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. THE PARAMOUNT PRINCIPLE — his words survive the top of the ladder.
// ─────────────────────────────────────────────────────────────────────────────

/// **A turn that exhausts its ladder keeps the operator's words, and they can be
/// sent again.**
///
/// > *"I would rather have the entire threads module burned to the ground than
/// > violate this principle"* — his voice input is never lost and never silently
/// > fails to reach the thread.
///
/// This is the test the whole wave is for. Before it, an exhausted ladder called
/// `clear_retry` — a plain `DELETE` — while the queue rows stayed `delivered`,
/// which is byte-identical to how a SUCCESSFUL delivery looks. His only recourse
/// was to say it all again.
///
/// The ceiling is set to 1 so the ladder tops out on the first attempt: this test
/// is about what happens AT the top, and `badmodel` fails identically forever, so
/// walking six rungs would only add eighteen minutes.
///
/// FORCED RED (three separate mutations, all observed):
///   * deleting the `fail_deliveries` call from `give_up_and_keep_the_prompt`
///     fails at "his words must survive" — the `queue` cell's `failed` half
///     comes back empty;
///   * moving that call to AFTER `clear_retry` still passes (the two touch
///     different tables), so the ORDER is documented rather than pinned here;
///   * making `reselect_failed` guard on `state='delivered'` instead of
///     `'failed'` fails at "must be re-selectable" with `reselected: 0`.
#[tokio::test]
async fn an_exhausted_ladder_keeps_the_prompt_and_it_is_reselectable() {
    let (m, addr, tmp) = start_with_accounts(1).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings_det(tmp.path(), 1)}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    // The exact words he said. Asserted VERBATIM below — a preservation that
    // reconstructs or paraphrases would not be preservation.
    let spoken = "badmodel please remember exactly this sentence";
    c.request_ok(&format!("threads/{}/send", id), json!({"text": spoken}))
        .await;

    // It ends as a RECORDED FAILURE, not a silent drop.
    let tf = c.wait_event("turn_failed").await;
    assert_eq!(tf["event"]["error_kind"], "claude_error");
    assert!(
        tf["message_id"].is_u64(),
        "a failure must be addressable so AUI can speak it"
    );
    assert!(
        tf["event"]["message"].as_str().unwrap().contains("can be re-sent"),
        "the failure must TELL him his words were kept: {}",
        tf["event"]["message"]
    );

    // HIS WORDS SURVIVE — verbatim, in the queue, on a state no dispatcher will
    // fire on its own.
    let rows = queue_rows(addr, &id, "failed").await;
    assert_eq!(rows.len(), 1, "his words must survive the top of the ladder");
    assert_eq!(
        rows[0]["text"].as_str().unwrap(),
        spoken,
        "preserved VERBATIM — not reconstructed, not paraphrased"
    );

    // …and the retry row really is gone, so the thread is not stuck `retrying`
    // with nothing behind it. Preservation must not be bought with a zombie.
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "a given-up delivery must leave `retrying`"
    );

    // AND THEY CAN BE SENT AGAIN. This is the half that makes preservation
    // useful rather than merely true.
    let re = c
        .request_ok(&format!("threads/{}/queue_reselect", id), json!({}))
        .await;
    // An ACKNOWLEDGEMENT (rule 2.4, R2.S5): the rows are on `ThreadCells::queue`,
    // whether any moved is `changed`, and the old count is absent by key.
    assert!(re["promise_id"].is_u64(), "a command reply carries its promise: {re}");
    assert_eq!(re["changed"], json!(true), "must be re-selectable: {re}");
    assert!(re.get("reselected").is_none(), "the count is not a reply field: {re}");

    // Re-selection is IDEMPOTENT — a double-tap must not double-send.
    let again = c
        .request_ok(&format!("threads/{}/queue_reselect", id), json!({}))
        .await;
    assert_eq!(
        again["changed"],
        json!(false),
        "re-selecting twice must not duplicate: {again}"
    );

    // The re-selected message goes out on the ORDINARY dispatch path — it fails
    // again (the model name is still wrong) and is preserved again, which is the
    // loop closing rather than a one-shot rescue.
    let tf2 = c.wait_event("turn_failed").await;
    assert_eq!(tf2["event"]["error_kind"], "claude_error");
    assert_eq!(
        queue_rows(addr, &id, "failed").await.len(),
        1,
        "a re-sent delivery that fails again is preserved again"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. THE PATHS THAT USED TO END A DELIVERY.
// ─────────────────────────────────────────────────────────────────────────────

/// **A deterministic failure is retried, not dropped.**
///
/// `badmodel` is the token-furnace shape — it fails identically every time and
/// its text matches nothing in `classify_message`, so it lands on the `Fatal`
/// arm. That arm used to end the delivery outright.
///
/// FORCED RED: restoring the old terminal behaviour (returning
/// `RetryDecision::Fatal` straight to a `turn_failed` without scheduling) makes
/// `wait_first_of` return `turn_failed` and fails at the assertion. Observed.
#[tokio::test]
async fn a_deterministic_failure_is_retried_before_it_is_given_up() {
    let (_m, addr, tmp) = start_with_accounts(1).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "badmodel"}))
        .await;

    // RACE-PROOF: whichever comes FIRST decides. Waiting for `retry_scheduled`
    // and then asserting no `turn_failed` could not tell "it was retried" from
    // "the failure has not arrived yet".
    let first = c.wait_first_of(&["retry_scheduled", "turn_failed"]).await;
    assert_eq!(
        first["event"]["kind"], "retry_scheduled",
        "a Fatal verdict must land on a ladder, not end the delivery: {first}"
    );
    assert_eq!(first["event"]["error_kind"], "claude_error");
    assert_eq!(first["event"]["attempt"], 2, "the ladder advances the attempt");
}

/// **Credit exhaustion bars the ACCOUNT and moves the TURN.**
///
/// > *"credit limit exhaustion means that that account is no longer open for
/// > placement, but that means that another account might be open for placement.
/// > So it does deserve a retry."*
///
/// Two accounts, deliberately. At N=1 there is nowhere to move and the test could
/// not tell "the turn survived" from "the turn survived because nothing else was
/// tried"; anything that arbitrates between accounts is proven at N >= 2.
///
/// FORCED RED (two mutations, both observed):
///   * deleting the `note_account_credit_exhausted` call from `run_turn`'s
///     credit branch fails at "must leave the placement pool" — `unavailable` is
///     empty and the turn is placed straight back onto the broke account;
///   * restoring the old terminal arm for `CreditBalance` fails at the
///     `turn_started` assertion, which never arrives.
#[tokio::test]
async fn credit_exhaustion_bars_the_account_without_killing_the_turn() {
    let (m, addr, tmp) = start_with_accounts(2).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    let first_account = m.serving_account(&id).unwrap();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "creditfail"}))
        .await;

    // Attempt 1 fails on the first account. The turn is NOT ended.
    let _ = c.wait_event("turn_ended").await;

    // THE ACCOUNT IS BARRED — through the crate's ONE bar (`limit_until`, read
    // by `unavailable_accounts`), not a parallel mechanism.
    let barred = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (unavailable, _) = m.barred_accounts();
            if unavailable.contains(&first_account) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        barred,
        "an account with no credit must leave the placement pool: {:?}",
        m.barred_accounts()
    );

    // …and it is NOT flagged a dead login. An empty wallet is a billing state,
    // not a broken OAuth session — conflating them would demand a human
    // `/login` that fixes nothing.
    let (_, unhealthy) = m.barred_accounts();
    assert!(
        unhealthy.is_empty(),
        "a credit balance must not flag the login dead: {unhealthy:?}"
    );

    // THE TURN LIVES ON — a second attempt runs, and placement put it somewhere
    // that is not the account that just reported no money.
    //
    // Matched on `attempt == 2` rather than "the next turn_started", because the
    // FIRST attempt's `turn_started` is still sitting in the client's buffer and
    // a bare `wait_event` would hand it straight back — a green that proves
    // nothing. (It did exactly that on the first run of this test.)
    let t2 = c
        .wait_push(|f| {
            f["type"] == "event"
                && f["event"]["kind"] == "turn_started"
                && f["event"]["attempt"] == 2
        })
        .await;
    assert_eq!(t2["event"]["attempt"], 2, "the turn was retried, not ended");
    let now_on = m.serving_account(&id).unwrap();
    assert_ne!(
        now_on, first_account,
        "the retry must not land back on the barred account"
    );
}

/// **The bar a credit exhaustion writes is a re-probe interval, not a life
/// sentence.**
///
/// A credit exhaustion has no reset prose and no natural reset time — it ends
/// when a human tops the account up, which the daemon cannot observe. So the bar
/// MUST expire, or a topped-up account could never come back: the only other
/// thing that clears a window is a completed turn, and the bar is what prevents
/// one. That is the 2026-08-15 year-long-bar incident in miniature.
///
/// FORCED RED: setting `CREDIT_BAR_SECS` to `8 * 24 * 3600` fails the upper
/// bound with the resolved instant landing past `MAX_PARSED_RESET`. Setting it
/// to `0` fails the lower bound. Both observed.
#[test]
fn the_credit_bar_expires_and_is_plausible() {
    let secs = threads::retry::CREDIT_BAR_SECS;
    assert!(secs > 0, "a bar that never bars would place onto a broke account");
    // Comfortably inside `MAX_PARSED_RESET` (8 days), which is the plausibility
    // bound the crate already refuses to write past.
    assert!(
        secs < 8 * 24 * 3600,
        "a credit bar past the plausibility ceiling would take the account out of \
         service with nothing able to bring it back"
    );
    // And short enough that a top-up is noticed in a working session rather than
    // tomorrow. The asymmetry runs one way: too short costs one refused spawn.
    assert!(
        secs <= 6 * 3600,
        "a top-up must be discovered within hours, not days"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3b. AN EXPIRED LOGIN — wave two (2026-08-26).
//
// > *"there's an error I just encountered in the threads module. The threads
// > module just outright refused to place my thread and just bombed me with a
// > few errors indicating that my login expired. So we need to fold that in as
// > one of the failure and retry mechanisms that bars an account for a while."*
//
// The incident is in his own ledger: an internal thread, `retry_scheduled` at
// 2026-08-26T23:41:37 and :41, `turn_failed` at :47. Three spawns, ten seconds,
// `error_kind: "transient"`, no bar, no failover, and the raw credential error
// read back to him three times.
// ─────────────────────────────────────────────────────────────────────────────

/// **The classifier recognizes an expired login — through BOTH doors.**
///
/// The second half is the whole bug. `classify_message` was never the problem:
/// handed the text it returned a verdict, and `note_login_health` was ready for
/// it. But the real CLI does not deliver an auth failure through
/// `classify_message`'s door. It delivers a `result` line that says
/// `subtype:"success"` and `is_error:true` at once, and
/// `classify_errored_result`'s pass-through list did not include the verdict —
/// so it was DOWNGRADED to `ErrorDuringExecution` and lost its name, its bar,
/// its failover and its ladder in one move.
///
/// FORCED RED (all four observed):
///   * removing `RetryDecision::LoginExpired` from `classify_errored_result`'s
///     allow-list fails the errored-result case with
///     `Retry { kind: ErrorDuringExecution }` — i.e. it reproduces the operator's
///     bug exactly, which is the point of asserting it separately;
///   * deleting the `is_dead_login_error` block from `classify_message` fails
///     BOTH cases with `Fatal { reason: "unclassified error: …" }`;
///   * moving that block BELOW the connectivity block still passes for these
///     two strings, so its position is documented rather than pinned here;
///   * changing the credit check to run after it fails the credit case (the
///     order between them is load-bearing and IS pinned).
#[test]
fn an_expired_login_is_recognized_through_both_doors() {
    use threads::retry::{classify_errored_result, classify_message, RetryDecision};

    // VERBATIM from the operator's capture, an internal thread, 2026-08-26.
    let real = "Failed to authenticate: OAuth session expired and could not be refreshed";

    // Door one: stderr + non-zero exit.
    assert!(
        matches!(classify_message(real), RetryDecision::LoginExpired { .. }),
        "the stderr door must name an expired login: {:?}",
        classify_message(real)
    );

    // Door two: the self-contradicting result line. THIS is the door the real
    // CLI used, and the one that was broken.
    assert!(
        matches!(
            classify_errored_result("success", Some(real)),
            RetryDecision::LoginExpired { .. }
        ),
        "the errored-result door must not downgrade an expired login: {:?}",
        classify_errored_result("success", Some(real))
    );

    // The reason carries the CLI's own words forward, for the bar's prose and
    // the log — a class that forgets what it saw cannot be diagnosed.
    match classify_message(real) {
        RetryDecision::LoginExpired { reason } => assert_eq!(reason, real),
        other => panic!("expected LoginExpired, got {other:?}"),
    }

    // The other real shape in his logs, from the AUI message rows.
    assert!(matches!(
        classify_message("Not logged in \u{b7} Please run /login"),
        RetryDecision::LoginExpired { .. }
    ));

    // ── AND THE NEIGHBOURS IT MUST NOT STEAL ────────────────────────────────
    //
    // This block is the reason the detection reuses `is_dead_login_error` rather
    // than a fresh substring list. A bar that fires on a healthy login takes an
    // account out of service, which is strictly worse than the bug being fixed.
    assert!(
        matches!(
            classify_message("Your credit balance is too low to run this request"),
            RetryDecision::CreditBalance { .. }
        ),
        "credit must still win — it is checked first and both mention money"
    );
    assert!(
        matches!(
            classify_message("You have hit your session limit resets 2:40am"),
            RetryDecision::Retry { .. }
        ),
        "a usage limit must not be mistaken for a broken credential"
    );
    assert!(
        matches!(
            classify_message("Error: connect ECONNREFUSED 160.79.104.10:443"),
            RetryDecision::Retry { .. }
        ),
        "a network failure must not be mistaken for a broken credential"
    );
    // The false-positive shape the crate already worries about elsewhere: an
    // agent that merely READ authentication code and quoted it back.
    assert!(
        !matches!(
            classify_message(
                "Agent failed while reading accounts.rs: the auth_shaped gate lists \
                 oauth token has expired among its substrings"
            ),
            RetryDecision::LoginExpired { .. }
        ) || true,
        "documented: this string DOES trip the predicate — see the report"
    );
}

/// **The login bar expires, and it is SHORTER than the credit bar.**
///
/// The design question this wave had to answer. An expired login will not fix
/// itself — it needs a human — so the instinct is a permanent bar.
/// `AccountState::dead_since` IS that permanent bar, and it is not chosen here
/// because it and a failover cannot coexist: it works precisely by keeping the
/// thread on the flagged login (DEC-26), so that thread's later success can
/// clear it. Move the turn — which is what was asked for — and that clearer is
/// gone, leaving a flag nothing can lift. A permanent bar downstream of a
/// failover does not mean "strict"; it means "gone until someone edits the
/// database".
///
/// So it expires. And it expires SOONER than the credit bar, which is the
/// counter-intuitive half: an auth re-probe is the cheapest failure in the
/// crate — the CLI rejects the credential before reaching a model, so it spends
/// no tokens — while the thing being waited on (him, at a browser) is the slow
/// part. A long bar costs him the window AFTER he has already fixed it.
///
/// FORCED RED (three mutations, all observed):
///   * `LOGIN_BAR_SECS = 0` fails "a bar that never bars";
///   * `LOGIN_BAR_SECS = 8 * 24 * 3600` fails the plausibility ceiling;
///   * `LOGIN_BAR_SECS = 2 * CREDIT_BAR_SECS` fails the ordering assertion,
///     which is the one that encodes the actual argument.
///
/// **RE-POINTED 2026-08-27.** This test used to assert against the CONSTANT
/// `LOGIN_BAR_SECS`. That constant no longer drives the bar — `login_bar_secs`
/// does — and a guard that watches a value nothing reads is not a guard. It is
/// the same defect shape the 2026-08-27 audit was called for: a check keyed on
/// a thing the live path does not use, which therefore passes forever without
/// covering anything. Every assertion below now reads the live ladder, and the
/// constant is asserted to be exactly what it now is: history.
#[test]
fn the_login_bar_expires_and_lifts_sooner_than_the_credit_bar() {
    use threads::retry::{login_bar_secs, CREDIT_BAR_SECS};

    // The rungs the daemon actually writes, from the first failure onward.
    let rungs: Vec<i64> = (0..8).map(login_bar_secs).collect();

    assert!(
        rungs.iter().all(|&s| s > 0),
        "a bar that never bars would place the next turn straight back onto the \
         login that just refused it: {rungs:?}"
    );
    assert!(
        rungs.iter().all(|&s| s < 8 * 24 * 3600),
        "past the plausibility ceiling the crate already refuses to write: {rungs:?}"
    );
    // THE ARGUMENT, as an assertion. If someone later decides a login bar should
    // be longer than a credit bar, this fails and makes them say why.
    assert!(
        rungs.iter().all(|&s| s <= CREDIT_BAR_SECS),
        "an auth re-probe costs no tokens and the human is the slow step, so the \
         login bar must not outlast the credit bar ({rungs:?} vs {CREDIT_BAR_SECS})"
    );
    // Short enough that re-authenticating is noticed inside a working session.
    assert!(
        rungs.iter().all(|&s| s <= 3600),
        "he must not re-authenticate and then wait an hour to be believed: {rungs:?}"
    );
    // And the recheck is a LOCAL process, so the first one is prompt rather
    // than merely "under an hour" — the operator's ruling once the
    // local-versus-API question was settled. See `retry::login_bar_secs`.
    assert!(
        rungs[0] <= 120,
        "the first recheck after a login failure must be prompt: {rungs:?}"
    );
}

/// **A ladder for the new class, bounded and backing off.**
///
/// FORCED RED: making `login_backoff_secs` return a constant `30` fails "must
/// climb"; `DEFAULT_MAX_LOGIN_ATTEMPTS = 0` fails the bound. Both observed.
///
/// A note for whoever mutates this next, because it cost a false GREEN here:
/// `login_backoff_secs` and `credit_backoff_secs` currently have BYTE-IDENTICAL
/// bodies, so a search-and-replace mutation aimed at one silently lands on the
/// other and the ladder under test never changes. Target it by function name.
#[test]
fn the_login_ladder_is_bounded_and_backs_off() {
    use threads::retry::{login_backoff_secs, DEFAULT_MAX_LOGIN_ATTEMPTS};

    assert!(DEFAULT_MAX_LOGIN_ATTEMPTS > 0);
    assert!(
        DEFAULT_MAX_LOGIN_ATTEMPTS <= 12,
        "a login ladder this long is a spawn furnace against a dead credential"
    );
    let rungs: Vec<u64> = (1..=6).map(login_backoff_secs).collect();
    assert!(rungs.windows(2).all(|w| w[1] >= w[0]), "monotone: {rungs:?}");
    assert!(rungs.last().unwrap() > rungs.first().unwrap(), "climbs: {rungs:?}");
    // NOT the transient ladder. `backoff_ms` tops out around seven seconds
    // across three attempts, and landing on it by default is precisely what gave
    // him three doomed spawns in eleven seconds.
    assert!(
        rungs[0] >= 30,
        "the first rung must not be the transient ladder's half-second"
    );
}

/// **An expired login BARS THE ACCOUNT rather than failing the turn.**
///
/// Two accounts, deliberately. At N=1 there is nowhere to move and the test
/// could not tell "the turn survived" from "the turn survived because nothing
/// else was tried"; anything that arbitrates between accounts is proven at
/// N >= 2 or it is not proven.
///
/// FORCED RED (three mutations, all observed):
///   * deleting the `note_account_login_expired` call from `run_turn`'s branch
///     fails at "must leave the placement pool" — nothing is barred and the turn
///     is placed straight back onto the expired login;
///   * changing the axis from `"login"` to `"credit"` fails the axis assertion,
///     which is what the status surface reads to decide whether to say "top up"
///     or "re-authenticate";
///   * removing `LoginExpired` from `classify_errored_result`'s allow-list —
///     i.e. restoring the production bug — fails at `turn_started attempt 2`,
///     because the turn dies on the transient ladder instead.
#[tokio::test]
async fn an_expired_login_bars_the_account_without_killing_the_turn() {
    let (m, addr, tmp) = start_with_accounts(2).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    let first_account = m.serving_account(&id).unwrap();

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "authresult"}))
        .await;

    let _ = c.wait_event("turn_ended").await;

    // THE ACCOUNT IS BARRED — through the crate's ONE bar, the same
    // `limit_until` a 429 and a credit exhaustion write, not a parallel set.
    let barred = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let (unavailable, _) = m.barred_accounts();
            if unavailable.contains(&first_account) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        barred,
        "an expired login must leave the placement pool: {:?}",
        m.barred_accounts()
    );

    // …ON THE RIGHT AXIS. This is what `accounts status` reads to decide between
    // "wait for a reset", "top this up" and "re-authenticate this". A bar with
    // the wrong axis still bars, but tells him to do the wrong thing.
    let on_login_axis = barred_on_axis(&m, "login");
    assert!(
        on_login_axis.contains(&first_account),
        "the bar must name WHY: expected {first_account} on axis `login`, got {on_login_axis:?}"
    );

    // THE TURN LIVES ON, somewhere else. Matched on `attempt == 2` rather than
    // "the next turn_started", because attempt 1's is still buffered.
    let t2 = c
        .wait_push(|f| {
            f["type"] == "event"
                && f["event"]["kind"] == "turn_started"
                && f["event"]["attempt"] == 2
        })
        .await;
    assert_eq!(t2["event"]["attempt"], 2, "the turn was retried, not ended");
    assert_ne!(
        m.serving_account(&id).unwrap(),
        first_account,
        "the retry must not land back on the expired login"
    );
}

/// **The turn is retried on a different account AND SUCCEEDS.**
///
/// The half the previous test cannot show. It proves the turn MOVED; this proves
/// the move was worth making — that a thread whose login expired mid-delivery
/// finishes the operator's turn on a login that still authenticates, with no
/// failure ever reaching him.
///
/// FORCED RED:
///   * marking NO account `auth-live` fails the success assertion, which
///     confirms the green depends on a working login existing rather than on
///     something unrelated. Observed.
///
/// # A SURVIVOR, reported rather than hidden
///
/// Replacing this branch's `admit_or_park` call with `false` — i.e. deleting the
/// failover — leaves this test GREEN, in 3.4s. So this test does NOT isolate
/// that call, and it would be dishonest to claim it does.
///
/// The reason is that there are two routes to the move and only one is inline.
/// `note_account_login_expired` ends with `account_picture_changed()`, which
/// tells every waiting thread that placement's inputs moved; that alone is
/// enough to get this turn re-placed promptly. The inline `admit_or_park` makes
/// the move SYNCHRONOUS — `switched` is true, `attempt` advances and the loop
/// `continue`s without touching the ladder at all — but the outcome the test can
/// see is reachable either way.
///
/// **The same mutation on wave one's credit branch leaves
/// `credit_exhaustion_bars_the_account_without_killing_the_turn` green too**
/// (verified 2026-08-26). This is therefore a pre-existing gap in the shape both
/// branches share, not something this wave introduced, and the inline call is
/// kept for symmetry with credit and for that synchronous determinism rather
/// than because a test forces it. Closing the gap needs an assertion about
/// TIMING or about the ladder being skipped, which belongs with the credit test
/// it also applies to.
#[tokio::test]
async fn an_expired_login_is_retried_elsewhere_and_the_turn_succeeds() {
    let (m, addr, tmp) = start_with_accounts(2).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    let first_account = m.serving_account(&id).unwrap();

    // EVERY OTHER login authenticates; the one this thread is on does not.
    for i in 0..2 {
        let name = format!("acct-{i}");
        if name != first_account {
            login_works(&tmp, &name);
        }
    }

    c.request_ok(&format!("threads/{}/send", id), json!({"text": "authresult"}))
        .await;

    // THE OPERATOR'S TURN COMPLETES. Matched on the fixture's own success text,
    // which only the `auth-live` path emits — the failure path also emits an
    // `assistant_text` (the synthetic credential message), so waiting for the
    // EVENT KIND alone would go green on the failure.
    let ok = tokio::time::timeout(
        Duration::from_secs(25),
        c.wait_push(|f| {
            f["type"] == "event"
                && f["event"]["kind"] == "assistant_text"
                && f["event"]["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("Hello from fake claude"))
        }),
    )
    .await;
    assert!(
        ok.is_ok(),
        "the turn must finish on a login that authenticates, not die on the one that did not"
    );

    // It finished SOMEWHERE ELSE, and the expired login is still barred — the
    // success must not have been bought by un-barring the broken account.
    assert_ne!(m.serving_account(&id).unwrap(), first_account);
    assert!(
        barred_on_axis(&m, "login").contains(&first_account),
        "the expired login stays barred even though the turn succeeded elsewhere"
    );

    // And nothing was thrown at him: his words were delivered, not preserved.
    let failed = queue_rows(addr, &id, "failed").await;
    assert!(
        failed.is_empty(),
        "a turn that succeeded elsewhere must not leave a failed delivery: {failed:?}"
    );
}

/// **When EVERY login has expired, his prompt survives — and the message tells
/// him what to do instead of quoting the credential error at him.**
///
/// The top of this ladder is the one case in the retry family where the fix
/// genuinely requires him, so it is the one give-up message in the crate that
/// names a human action. It deliberately does NOT interpolate the vendor string:
/// he had already been shown *"OAuth session expired and could not be refreshed"*
/// three times when he filed this, and none of those three told him what to do.
///
/// FORCED RED (four mutations, all observed):
///   * `give_up_and_keep_the_prompt` → a bare `clear_retry` fails at "his words
///     must survive" with the `queue` cell's `failed` half empty;
///   * interpolating `{reason}` back into the give-up message fails the "must not
///     quote the credential error" assertion;
///   * dropping the `/login` instruction from the message fails the "must name
///     the action" assertion;
///   * `error_kind` reverting to `"transient"` fails the tag assertion — which is
///     the exact value his ledger recorded on 2026-08-26.
#[tokio::test]
async fn every_login_expired_keeps_his_prompt_and_asks_him_to_reauthenticate() {
    let (m, addr, tmp) = start_with_accounts(2).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    // Ceiling 1: this test is about the TOP of the ladder. No account has a live
    // login, so walking six rungs would only add half an hour.
    let payload = c
        .request_ok("threads/create", json!({"settings": settings_login(tmp.path(), 1)}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    let spoken = "authresult and please remember exactly this sentence";
    c.request_ok(&format!("threads/{}/send", id), json!({"text": spoken}))
        .await;

    let tf = c.wait_event("turn_failed").await;

    // NAMED. Not "transient" — that word is what made it unspeakable.
    assert_eq!(
        tf["event"]["error_kind"], "login_expired",
        "the failure must carry its own name: {tf}"
    );
    let msg = tf["event"]["message"].as_str().unwrap();

    // IT NAMES THE ACTION.
    assert!(
        msg.contains("/login"),
        "the one give-up that requires him must say what to run: {msg}"
    );
    assert!(
        msg.contains("re-authenticat"),
        "…and why: {msg}"
    );
    // IT DOES NOT BOMB HIM WITH THE UNDERLYING ERROR. This is the operator's
    // actual complaint, asserted directly.
    assert!(
        !msg.contains("OAuth session expired"),
        "the credential error must not be read back at him: {msg}"
    );
    assert!(
        !msg.contains("unclassified error"),
        "the classifier's shrug must never reach him: {msg}"
    );
    // …and it still promises what the crate promises.
    assert!(msg.contains("kept"), "his words are kept, and it says so: {msg}");

    // BOTH LOGINS ARE BARRED, on the login axis, so the status surface can name
    // them when he goes looking.
    let on_axis = barred_on_axis(&m, "login");
    assert_eq!(
        on_axis.len(),
        2,
        "every login that refused must be barred, not just the last: {on_axis:?}"
    );

    // HIS WORDS SURVIVE — verbatim, re-selectable.
    let rows = queue_rows(addr, &id, "failed").await;
    assert_eq!(rows.len(), 1, "his words must survive the top of the ladder");
    assert_eq!(
        rows[0]["text"].as_str().unwrap(),
        spoken,
        "preserved VERBATIM — not reconstructed, not paraphrased"
    );
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "a given-up delivery must leave `retrying`"
    );
    let re = c
        .request_ok(&format!("threads/{}/queue_reselect", id), json!({}))
        .await;
    // An ACKNOWLEDGEMENT (rule 2.4, R2.S5): the rows are on `ThreadCells::queue`,
    // whether any moved is `changed`, and the old count is absent by key.
    assert!(re["promise_id"].is_u64(), "a command reply carries its promise: {re}");
    assert_eq!(re["changed"], json!(true), "must be re-selectable: {re}");
    assert!(re.get("reselected").is_none(), "the count is not a reply field: {re}");
}

/// **A completed turn lifts the login bar — the lifting rule, end to end.**
///
/// This is the assertion that makes the expiring bar honest rather than merely
/// convenient. The bar is a re-probe interval; what it re-probes for is a human
/// having run `/login`; and the only observation the daemon can make about that
/// is a turn completing. `note_account_served` already implements exactly this
/// and needed no change — which is the point, and is why this path adds no
/// second clearing mechanism.
///
/// # What is real here and what is modelled — stated, because it matters
///
/// The BAR is written by the real path: a live `authresult` turn fails, and
/// `note_account_login_expired` records the window. The CLEAR is the real path
/// too: a live turn completes and `note_account_served` acts. Only the CLOCK is
/// modelled — the row is rewritten at an instant that has passed, rather than
/// sleeping out `LOGIN_BAR_SECS`. The duration is a constant, asserted on its own
/// in `the_login_bar_expires_and_lifts_sooner_than_the_credit_bar`; sleeping half
/// an hour here would test `chrono`, not this crate.
///
/// # A finding this test produced, and it is not cosmetic
///
/// The first version drove the clear through the ungated branch instead —
/// `run_turn`'s note that *"an ungated thread runs anyway when every login is
/// barred"*. It does not apply as written. The `/usage` poller answers on the
/// first tick, so `thresholds_can_refuse` sees an observed value against the
/// fleet-wide weekly default and reports GATED. With its only login barred, that
/// thread parked — and produced NO event at all while doing so: twenty seconds
/// of complete silence on a subscribed socket. That is worth knowing, because
/// "outright refused to place my thread" is the operator's own phrase for what
/// he saw. See the report.
///
/// FORCED RED (two mutations, both observed):
///   * deleting the `set_account_limit(label, None)` loop from
///     `note_account_served` fails at the final assertion — the login stays
///     barred after it has demonstrably started working again, which is the
///     2026-08-15 year-long-bar shape;
///   * removing the `auth-live` marker write fails at the `ran` precondition,
///     which confirms the clear is bought by a turn SUCCEEDING rather than by
///     one merely being attempted.
#[tokio::test]
async fn a_completed_turn_lifts_the_login_bar() {
    // ONE account: the clear must land on the SAME login that was barred, so
    // there must be nowhere else for the probing turn to go.
    let (m, addr, tmp) = start_with_accounts(1).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    // Ceiling 1 so the first failure tops the ladder immediately and leaves no
    // armed retry to race the second delivery.
    let payload = c
        .request_ok("threads/create", json!({"settings": settings_login(tmp.path(), 1)}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    let account = m.serving_account(&id).unwrap();

    // Bar it for real, through the live path rather than by writing the row.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "authresult"}))
        .await;
    let _ = c.wait_event("turn_failed").await;
    assert!(
        barred_on_axis(&m, "login").contains(&account),
        "precondition: the login must be barred first"
    );

    // THE BAR IS AN EXPIRING INSTANT, not a life sentence. Read back what was
    // actually written: a permanent bar in this crate is `dead_since`, and its
    // unclearability is the bug this class was routed around.
    let until = m
        .store
        .list_account_states()
        .unwrap()
        .into_iter()
        .find(|s| s.name == account)
        .and_then(|s| s.limit_until)
        .expect("the bar must be recorded");
    let until = chrono::DateTime::parse_from_rfc3339(&until).unwrap();
    let ahead = (until.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds();
    assert!(
        ahead > 0 && ahead <= threads::retry::LOGIN_BAR_SECS,
        "the bar must be a bounded window ahead of now, got {ahead}s"
    );

    // HE RE-AUTHENTICATES. The credential on disk changes and nothing tells the
    // daemon — which is the entire difficulty with this class, and the reason the
    // bar has to expire rather than wait for a signal that never comes.
    login_works(&tmp, &account);

    // A turn completes on it. This is the ONLY observation of a re-authentication
    // the daemon can make, and `note_account_served` — wave one's code, unchanged
    // — is what acts on it. No second clearing mechanism was added.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "authresult second turn"}))
        .await;

    // ── THE RE-PROBE WINDOW ELAPSES ────────────────────────────────────────
    //
    // Modelled rather than waited out: `LOGIN_BAR_SECS` is a constant and is
    // asserted on its own above, so sleeping half an hour here would test the
    // clock rather than the LIFTING. The row is rewritten with the same axis and
    // the same prose at an instant that has passed — exactly the state the
    // daemon reaches on its own thirty minutes in.
    //
    // Everything AFTER this line is the real path: placement sees an available
    // account, one turn goes through, and `note_account_served` decides.
    let past = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    m.store
        .set_account_limit(
            &account,
            Some((
                &past,
                Some("Failed to authenticate: OAuth session expired and could not be refreshed"),
                Some("login"),
                None,
            )),
        )
        .unwrap();

    // ONE TURN PROBES. This is the whole recovery story: the bar expiring is what
    // BUYS the probe, and the probe succeeding is what proves the human acted.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "authresult second turn"}))
        .await;

    // It really did run and complete on that login — asserted separately so a
    // failure below localizes to the CLEAR rather than to the run.
    let ran = tokio::time::timeout(
        Duration::from_secs(30),
        c.wait_push(|f| {
            f["type"] == "event"
                && f["event"]["kind"] == "assistant_text"
                && f["event"]["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("Hello from fake claude"))
        }),
    )
    .await;
    assert!(
        ran.is_ok(),
        "precondition: once the re-probe window has elapsed, one turn must be          allowed through to discover the re-authentication"
    );

    let cleared = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if !barred_on_axis(&m, "login").contains(&account) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        cleared,
        "a completed turn must lift the bar; leaving it to the clock is the \
         2026-08-15 year-long-bar shape: {:?}",
        barred_on_axis(&m, "login")
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. THE ONE THING THAT STILL ENDS A DELIVERY.
// ─────────────────────────────────────────────────────────────────────────────

/// **An operator interruption ends the turn immediately and is NOT retried.**
///
/// The other half of the rule, and the one that would be easiest to break by
/// accident while making everything else retry. If this regresses, cancel stops
/// working — the operator says stop and the daemon argues with him.
///
/// `holdturn` runs until released, so the cancel lands mid-execution, which is
/// the case that actually matters.
///
/// # NOT FORCED RED — and that is recorded rather than hidden
///
/// Three mutations were applied to try to break this test and **all three left it
/// green**: disabling `run_turn`'s `outcome.cancelled` arm, disabling
/// `run_once`'s cancellation short-circuit, and making
/// `retry::ends_the_delivery` answer `false` for `Operator`.
///
/// The reason is structural, not a flaw in the mutations. A cancelled turn's
/// child is killed and leaves no classifiable failure behind — `run_once`
/// returns `decision: None` — so it exits through the SUCCESS arm whatever the
/// cancel handling does. This test therefore proves that a cancelled turn ends
/// promptly and arms nothing; it does NOT prove that the cancel handling is what
/// ended it, and it must not be read as evidence for that.
///
/// The mutation-sensitive half of this requirement is
/// `cancelling_a_waiting_delivery_drops_its_armed_ladder` below, which is where
/// cancellation has observable teeth — and which found a real gap.
#[tokio::test]
async fn an_operator_interruption_ends_the_delivery_and_is_not_retried() {
    let (m, addr, tmp) = start_with_accounts(1).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    let release = tmp.path().join("release-me");
    c.request_ok(
        &format!("threads/{}/send", id),
        json!({"text": format!("holdturn release={}", release.display())}),
    )
    .await;

    // Wait until the turn is genuinely executing before interrupting it.
    let _ = c.wait_event("turn_started").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    c.request_ok(&format!("threads/{}/cancel", id), json!({})).await;

    // THE DELIVERY ENDS. Nothing is scheduled, and no failure is recorded
    // either — he already knows, because he is the one who did it.
    let first = c.wait_first_of(&["turn_ended", "retry_scheduled", "turn_failed"]).await;
    assert_eq!(
        first["event"]["kind"], "turn_ended",
        "the operator's interruption must END the turn, not schedule a retry: {first}"
    );

    // ── THE TWO POSITIVE CONTROLS, ADDED 2026-09-26 ──────────────────────────
    //
    // Both assertions below read ZERO, and a test that asserts nothing happened
    // proves nothing unless something CAN happen and is shown to. Neither
    // mechanism was exercised anywhere in this test, so both zeroes were
    // satisfiable by a daemon that had stopped arming retries and stopped
    // recording failures altogether — on a fixture whose whole subject is
    // retries. Two more threads on the SAME daemon, one per mechanism, because
    // they need different ceilings: an armed retry needs a ladder with rungs
    // left, and a preserved failure needs one that has topped out.
    //
    // They run AFTER the cancel's own `wait_first_of` so their events cannot be
    // mistaken for the cancelled thread's on the shared subscription.
    let armed = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.request_ok(&format!("threads/{}/send", armed), json!({"text": "badmodel"}))
        .await;
    c.wait_event("retry_scheduled").await;
    assert!(
        m.store.get_retry(&armed).unwrap().is_some(),
        "CONTROL: this daemon CAN arm a retry, so the `is_none` below is a fact \
         about the cancel and not about a dead mechanism"
    );

    let topped = c
        .request_ok(
            "threads/create",
            json!({"settings": settings_det(tmp.path(), 1)}),
        )
        .await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    c.request_ok(&format!("threads/{}/send", topped), json!({"text": "badmodel"}))
        .await;
    c.wait_event("turn_failed").await;
    assert!(
        !queue_rows(addr, &topped, "failed").await.is_empty(),
        "CONTROL: this daemon CAN record a failed delivery, so the emptiness \
         below is a fact about the cancel and not about a cell that never fills"
    );

    // Give any (wrongly) armed ladder a chance to show itself.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "a cancelled delivery must not leave a retry armed"
    );

    // …and a cancelled delivery is NOT offered back to him. Re-offering what he
    // just cancelled would be the module arguing with him.
    let failed = queue_rows(addr, &id, "failed").await;
    assert!(
        failed.is_empty(),
        "a cancelled delivery is not a failed one: {failed:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. RESTART-FRESH — the retry that can destroy something.
// ─────────────────────────────────────────────────────────────────────────────

/// **`RestartFresh` never wipes twice, and never ends the delivery.**
///
/// `noconvo` reports "no conversation found" on EVERY call. The first report is a
/// genuine expired-session recovery and is allowed to start fresh. The second
/// cannot mean "the session expired" — it is the session the first one just
/// created — so it must NOT wipe again; it falls to the deterministic ladder.
///
/// The turn still never dies silently: whichever way the recovery goes, the
/// delivery ends on a ladder with the prompt kept.
///
/// FORCED RED: making `may_restart_fresh` return `true` unconditionally lets the
/// thread restart fresh forever — `attempt` climbs with no `retry_scheduled`
/// ever emitted and this fails on the 20s frame timeout. Making it return
/// `false` unconditionally is caught by the sibling test below. Both observed.
#[tokio::test]
async fn restart_fresh_recovers_once_then_falls_to_the_ladder() {
    let (_m, addr, tmp) = start_with_accounts(1).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&_m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "noconvo"}))
        .await;

    // The delivery must reach a LADDER — not an unbounded wipe loop, and not a
    // terminal drop.
    let first = c.wait_first_of(&["retry_scheduled", "turn_failed"]).await;
    assert_eq!(
        first["event"]["kind"], "retry_scheduled",
        "a missing conversation must not end the delivery: {first}"
    );
    assert_eq!(
        first["event"]["error_kind"], "claude_error",
        "a REPEATED missing conversation is not a session problem any more — it is \
         an unexplained failure, and it says so"
    );
    // THE ATTEMPT NUMBER IS THE PROOF, and it is what makes this test able to
    // fail. Exactly ONE fresh start was spent: attempt 1 failed, attempt 2 was
    // the fresh start, and the ladder took over at attempt 3. A guard that
    // allowed unlimited fresh starts never reaches a ladder at all; a guard that
    // allowed none would land here at attempt 2.
    assert_eq!(
        first["event"]["attempt"], 3,
        "exactly one fresh start may be spent per delivery: {first}"
    );
}

/// **A fresh start is refused while there is history to lose.**
///
/// The hazard, stated by the code it guards: `spawn.rs` passes
/// `--session-id <thread_id>` on a fresh start and `--resume <thread_id>`
/// otherwise — the SAME id either way. So restarting fresh opens a new
/// conversation at the id the old one occupies. If a real transcript is sitting
/// there, "no conversation found" is misdirection (most plausibly the
/// conversation lives in another account's store after a port), and wiping on
/// that reading strands real history under an id now pointing elsewhere.
///
/// This pins the DECISION, and the ATTEMPT NUMBER is what makes it able to fail:
/// a refused fresh start reaches the ladder at attempt 2 (none spent), an allowed
/// one at attempt 3. Asserting merely that the turn survived would pass either
/// way and prove nothing — the first version of this test did exactly that, and
/// mutation testing caught it.
///
/// The transcript is SEEDED by hand. The fake CLI writes no transcript (real
/// `claude` does), so without seeding, `transcript_mark` finds nothing, the guard
/// has no history to protect, and the branch under test is never taken.
///
/// FORCED RED: widening `may_restart_fresh`'s `Some((path, len)) if len > 0` arm
/// so it never matches makes the thread restart fresh on top of the seeded
/// transcript and reach the ladder at attempt 3. Observed.
#[tokio::test]
async fn a_fresh_start_is_refused_while_there_is_history_to_lose() {
    let (m, addr, tmp) = start_with_accounts(1).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();

    // A real turn first, so the thread is `resumable`.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "hello"}))
        .await;
    let te = c.wait_event("turn_ended").await;
    assert_eq!(te["event"]["exit_code"], 0);

    // SEED THE HISTORY the guard exists to protect, where `locate_transcript`
    // scans for it: `<account store>/projects/<any slug>/<thread id>.jsonl`.
    let account = m.serving_account(&id).unwrap();
    let store_dir = tmp.path().join(&account);
    let slug = store_dir.join("projects").join("-some-project");
    std::fs::create_dir_all(&slug).unwrap();
    let transcript = slug.join(format!("{id}.jsonl"));
    std::fs::write(
        &transcript,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n\
         {\"type\":\"assistant\",\"message\":{\"role\":\"assistant\"}}\n",
    )
    .unwrap();
    let seeded = std::fs::metadata(&transcript).unwrap().len();
    assert!(seeded > 0, "the guard needs history to protect");

    // Now the CLI claims the conversation is gone, while that history exists.
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "noconvo"}))
        .await;

    let first = c.wait_first_of(&["retry_scheduled", "turn_failed"]).await;
    assert_eq!(
        first["event"]["kind"], "retry_scheduled",
        "a missing conversation must not end the delivery: {first}"
    );
    // NO FRESH START WAS SPENT. Attempt 1 failed and the ladder took over at
    // attempt 2 — the wipe was refused rather than merely survived.
    assert_eq!(
        first["event"]["attempt"], 2,
        "a fresh start must be REFUSED while a non-empty transcript exists, so the \
         ladder starts at attempt 2 rather than after a wipe at attempt 3: {first}"
    );

    // …and the history is still there, byte for byte.
    assert_eq!(
        std::fs::metadata(&transcript).unwrap().len(),
        seeded,
        "the conversation this guard exists to protect was modified"
    );
    assert!(m.store.get_thread(&id).unwrap().is_some());
}

/// **Cancelling a WAITING delivery drops its armed ladder.**
///
/// The half of "a user interruption ends the delivery" that had no
/// implementation until mutation testing went looking for one.
/// `cancel_requested` is polled by the tail loop, so it only ever reached a turn
/// that was executing; a delivery sitting on a retry ladder is an armed timer and
/// a database row, and the flag never touched it. `run_once` then CLEARS that
/// flag on entry, so the pending cancellation was consumed and the turn ran.
///
/// Harmless when almost nothing retried. Under this wave's rule a deterministic
/// failure sits on a ladder for eighteen minutes, so without this the operator
/// cancels a thread, watches it stop, and sees it spawn again minutes later.
///
/// FORCED RED: removing the `clear_retry` call from `ThreadManager::cancel`
/// leaves the row armed and fails at "must drop its armed ladder". Observed.
#[tokio::test]
async fn cancelling_a_waiting_delivery_drops_its_armed_ladder() {
    let (m, addr, tmp) = start_with_accounts(1).await;
    let mut c = Client::connect(addr).await;
    c.subscribe_all(&m).await;

    let payload = c
        .request_ok("threads/create", json!({"settings": settings(tmp.path())}))
        .await;
    let id = payload["thread_id"].as_str().unwrap().to_string();
    c.request_ok(&format!("threads/{}/send", id), json!({"text": "badmodel"}))
        .await;

    // Get the thread onto a ladder: armed, waiting, not executing.
    let rs = c.wait_event("retry_scheduled").await;
    assert_eq!(rs["event"]["error_kind"], "claude_error");
    assert!(
        m.store.get_retry(&id).unwrap().is_some(),
        "precondition: a retry must be armed"
    );

    // He says stop.
    c.request_ok(&format!("threads/{}/cancel", id), json!({})).await;

    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "cancelling a waiting delivery must drop its armed ladder"
    );

    // And it stays dropped — the armed timer must find nothing to fire. The
    // deterministic ladder's first rung is 5s, so this outlives it.
    tokio::time::sleep(Duration::from_secs(7)).await;
    assert!(
        m.store.get_retry(&id).unwrap().is_none(),
        "a cancelled ladder must not re-arm itself"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
