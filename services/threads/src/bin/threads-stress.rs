//! `threads-stress` — the STRESS-TEST DRIVER (round-7 item 8).
//!
//! The operator's instruction: *"I can stress test the starting of a thread
//! based off of thresholds by just setting the thresholds lower than what the
//! values are right now ... you need to not execute things yourself ... You
//! need to call a script."*
//!
//! So this is that script. One invocation:
//!
//! 1. takes the exclusive stress lock and REFUSES to run if any other thread is
//!    active — exactly one execution at a time, never a surprise second turn
//!    landing on the operator's live usage;
//! 2. sets a threshold and/or a budget value on a key (the whole point: a
//!    threshold a hair under the current gauge reading, a tiny budget, a kill
//!    toggle);
//! 3. sends ONE message to a designated test thread and watches the ledger;
//! 4. reports the outcome — `gated`, `parked`, `tripped`, `killed`, `finished`,
//!    `failed` or `timeout` — as JSON on stdout, and puts the configuration
//!    back the way it found it.
//!
//! It does not decide what to test and it never loops. The coordinator supplies
//! the numbers; this binary makes the run reproducible, exclusive and legible.
//!
//! **Restore, now that budget keys are DISABLED and never deleted** (Q-34, msg
//! #751): a key the driver MINTED cannot be removed afterwards, so restore
//! unpoints the test thread from it and DISABLES it — inert, pointed at by
//! nothing, and still fully queryable, which is what the operator asked for.
//! The mirror image of that is on the way in: a key that already exists and is
//! disabled is ENABLED for the duration of the run (a disabled key denies every
//! turn, so the run would otherwise only ever measure the disable gate) and put
//! back exactly as it was found. Both directions are reported in `applied` /
//! `restored`, because silently flipping an operator's deny switch is not
//! something a driver should do quietly.
//!
//! ```text
//! threads-stress --new-thread --cwd /tmp \
//!   --key stress --accounts default --session-pct 0.5 --on-trip kill \
//!   --text "count to five"
//!
//! threads-stress --thread <id> --threshold-fable 55 --text "hello"
//! ```
//!
//! Exit codes: `0` ran and observed · `2` bad usage · `3` REFUSED (something
//! else is active, or another driver holds the lock) · `4` timed out · `1` error.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use futures::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const USAGE: &str = "\
threads-stress — one exclusive budget/threshold stress run against the daemon

  CONNECTION
    --url <ws://host:port/ws>   default $THREADS_STRESS_URL, else ws://127.0.0.1:3653/ws
    --secret <s>                default $THREADS_SECRET
    --lock <path>               default $THREADS_HOME/stress.lock, else ~/.threads/stress.lock

  WHAT TO SET (the value under test)
    --key <budget-key>          budget key to configure and/or assign
    --accounts <a,b>            REQUIRED when creating a key (never defaulted to all)
    --session-pct <n>           budget on the session axis
    --weekly-pct <n>            budget on the weekly all-model axis
    --fable-pct <n>             budget on the weekly fable axis
    --cash-usd <n>              budget on the cash axis, in DOLLARS
    --on-trip <kill|finish>     per-budget teardown toggle (default finish)
    --threshold-session <n>     per-thread gate, percent
    --threshold-weekly <n>      per-thread gate, percent
    --threshold-fable <n>       per-thread gate, percent
    --threshold-cash <n>        per-thread gate, DOLLARS
    --clear-thresholds          clear this thread's thresholds first

  THE RUN
    --thread <id>               use an existing designated test thread
    --new-thread                create a throwaway thread instead
    --cwd <dir>                 cwd for --new-thread (default $PWD)
    --model <m>                 model for --new-thread (default sonnet)
    --text <prompt>             the one message to send
    --timeout-secs <n>          give up after this long (default 300)
    --settle-secs <n>           how long a park must hold to count (default 5)

  SAFETY
    --allow-active              do NOT refuse when other threads are busy (never use live)
    --no-restore                leave the budget/threshold values in place afterwards
    --dry-run                   check exclusivity and report readiness; send nothing
";

/// Every exit path this binary has runs through here, and every one of them
/// must release the run lock first. `std::process::exit` does NOT run
/// destructors, so a `die()` after the lock was taken used to leave the lock
/// file behind holding a dead PID — observed 2026-07-29 against the DEFAULT
/// path, which put a stray `~/.threads/stress.lock` under the one directory
/// this tooling is told never to touch, and would have refused every later run
/// until someone deleted it by hand.
fn die(code: i32, msg: &str) -> ! {
    eprintln!("threads-stress: {msg}");
    release_lock();
    std::process::exit(code)
}

#[derive(Default)]
struct Args {
    url: Option<String>,
    secret: Option<String>,
    lock: Option<String>,
    key: Option<String>,
    accounts: Option<Vec<String>>,
    session_pct: Option<f64>,
    weekly_pct: Option<f64>,
    fable_pct: Option<f64>,
    cash_usd: Option<f64>,
    on_trip: Option<String>,
    th_session: Option<f64>,
    th_weekly: Option<f64>,
    th_fable: Option<f64>,
    th_cash: Option<f64>,
    clear_thresholds: bool,
    thread: Option<String>,
    new_thread: bool,
    cwd: Option<String>,
    model: Option<String>,
    text: Option<String>,
    timeout_secs: u64,
    settle_secs: u64,
    allow_active: bool,
    no_restore: bool,
    dry_run: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        timeout_secs: 300,
        settle_secs: 5,
        ..Default::default()
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let next = |i: &mut usize, flag: &str| -> String {
        *i += 1;
        argv.get(*i)
            .cloned()
            .unwrap_or_else(|| die(2, &format!("{flag} needs a value")))
    };
    let num = |s: String, flag: &str| -> f64 {
        s.parse()
            .unwrap_or_else(|_| die(2, &format!("{flag} needs a number, got {s:?}")))
    };
    while i < argv.len() {
        let flag = argv[i].clone();
        match flag.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0)
            }
            "--url" => a.url = Some(next(&mut i, &flag)),
            "--secret" => a.secret = Some(next(&mut i, &flag)),
            "--lock" => a.lock = Some(next(&mut i, &flag)),
            "--key" => a.key = Some(next(&mut i, &flag)),
            "--accounts" => {
                a.accounts = Some(
                    next(&mut i, &flag)
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                )
            }
            "--session-pct" => a.session_pct = Some(num(next(&mut i, &flag), &flag)),
            "--weekly-pct" => a.weekly_pct = Some(num(next(&mut i, &flag), &flag)),
            "--fable-pct" => a.fable_pct = Some(num(next(&mut i, &flag), &flag)),
            "--cash-usd" => a.cash_usd = Some(num(next(&mut i, &flag), &flag)),
            "--on-trip" => a.on_trip = Some(next(&mut i, &flag)),
            "--threshold-session" => a.th_session = Some(num(next(&mut i, &flag), &flag)),
            "--threshold-weekly" => a.th_weekly = Some(num(next(&mut i, &flag), &flag)),
            "--threshold-fable" => a.th_fable = Some(num(next(&mut i, &flag), &flag)),
            "--threshold-cash" => a.th_cash = Some(num(next(&mut i, &flag), &flag)),
            "--clear-thresholds" => a.clear_thresholds = true,
            "--thread" => a.thread = Some(next(&mut i, &flag)),
            "--new-thread" => a.new_thread = true,
            "--cwd" => a.cwd = Some(next(&mut i, &flag)),
            "--model" => a.model = Some(next(&mut i, &flag)),
            "--text" => a.text = Some(next(&mut i, &flag)),
            "--timeout-secs" => a.timeout_secs = num(next(&mut i, &flag), &flag) as u64,
            "--settle-secs" => a.settle_secs = num(next(&mut i, &flag), &flag) as u64,
            "--allow-active" => a.allow_active = true,
            "--no-restore" => a.no_restore = true,
            "--dry-run" => a.dry_run = true,
            other => die(2, &format!("unknown flag {other}\n\n{USAGE}")),
        }
        i += 1;
    }
    if a.thread.is_none() && !a.new_thread {
        die(2, &format!("one of --thread or --new-thread is required\n\n{USAGE}"));
    }
    a
}

/// The lock file THIS process created, if any — the one thing [`die`] has to
/// clean up on its way out. Only ever `Some` when `create_new` actually made
/// the file, so a run that REFUSED because another driver already held the
/// lock can never delete that other run's lock.
static HELD_LOCK: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// Remove this process's own lock file, once. Idempotent: whichever of
/// [`Lock`]'s `Drop` and [`die`] runs first takes the path out.
fn release_lock() {
    let taken = HELD_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(p) = taken {
        let _ = std::fs::remove_file(p);
    }
}

/// The exclusive run lock. `create_new` is the whole mechanism: if the file is
/// there, another driver owns the daemon and we refuse rather than queue.
struct Lock;

impl Lock {
    fn take(path: std::path::PathBuf) -> std::io::Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(|mut f| {
                use std::io::Write;
                let _ = writeln!(f, "{}", std::process::id());
                *HELD_LOCK.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
                Lock
            })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        release_lock();
    }
}

struct Client {
    wr: SplitSink<Ws, Message>,
    rd: SplitStream<Ws>,
    next_id: u64,
    /// The highest ledger `seq` already read for the thread under test. The
    /// run reads the ledger by polling `threads/{id}/history{after_seq}` from
    /// here; see [`Client::watch_from_tail`].
    cursor: u64,
    /// History events read but not yet handed to the run, in seq order.
    events: VecDeque<Value>,
}

impl Client {
    async fn connect(url: &str, secret: Option<&str>) -> Client {
        let (ws, _) = connect_async(url)
            .await
            .unwrap_or_else(|e| die(1, &format!("connecting to {url}: {e}")));
        let (wr, rd) = ws.split();
        let mut c = Client {
            wr,
            rd,
            next_id: 0,
            cursor: 0,
            events: VecDeque::new(),
        };
        let welcome = c.next_frame(Duration::from_secs(10)).await;
        if welcome["auth_required"] == json!(true) {
            let Some(s) = secret else {
                die(2, "the daemon requires auth; pass --secret or set THREADS_SECRET")
            };
            c.send_raw(json!({"type": "auth", "secret": s})).await;
            let ack = c.next_frame(Duration::from_secs(10)).await;
            if ack["ok"] != json!(true) {
                die(1, "auth rejected");
            }
        }
        c
    }

    async fn send_raw(&mut self, v: Value) {
        if let Err(e) = self.wr.send(Message::Text(v.to_string())).await {
            die(1, &format!("sending frame: {e}"));
        }
    }

    async fn next_frame(&mut self, timeout: Duration) -> Value {
        loop {
            let msg = match tokio::time::timeout(timeout, self.rd.next()).await {
                Err(_) => die(4, "timed out waiting for the daemon"),
                Ok(None) => die(1, "the daemon closed the socket"),
                Ok(Some(Err(e))) => die(1, &format!("socket error: {e}")),
                Ok(Some(Ok(m))) => m,
            };
            match msg {
                Message::Text(t) => {
                    return serde_json::from_str(&t)
                        .unwrap_or_else(|e| die(1, &format!("bad frame from daemon: {e}")))
                }
                Message::Close(_) => die(1, "the daemon closed the socket"),
                _ => continue,
            }
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = format!("s{}", self.next_id);
        self.send_raw(json!({"type": "request", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let f = self.next_frame(Duration::from_secs(120)).await;
            if f["type"] == "response" && f["correlate"] == json!(id) {
                return f["outcome"].clone();
            }
            // Not ours: the chassis sends nothing unrequested to a connection
            // that holds no shape subscription, so this is only ever a frame
            // for a request this client already gave up on.
        }
    }

    async fn request_ok(&mut self, method: &str, params: Value) -> Value {
        let o = self.request(method, params.clone()).await;
        if o["type"] != json!("success") {
            die(
                1,
                &format!(
                    "{method} failed: {}",
                    o["error"].as_str().unwrap_or("unknown error")
                ),
            );
        }
        o["payload"].clone()
    }

    /// **START READING THE LEDGER FROM ITS CURRENT TAIL.**
    ///
    /// This sent `{"type":"subscribe","threads":[id]}` and read `event` frames
    /// off the legacy firehose until R2.S4 retired it. The ledger is the same
    /// ledger either way, and `threads/{id}/history{after_seq}` is its
    /// request-shaped read, so the run now polls that: everything after the
    /// tail as it stood BEFORE the send is what the run is owed, exactly what
    /// a subscribe placed before the send delivered.
    async fn watch_from_tail(&mut self, thread_id: &str) {
        loop {
            let page = self
                .request_ok(
                    &format!("threads/{thread_id}/history"),
                    json!({ "after_seq": self.cursor, "limit": 500 }),
                )
                .await;
            let next = page["next_after"].as_u64().unwrap_or(self.cursor);
            if next <= self.cursor {
                return;
            }
            self.cursor = next;
        }
    }

    /// The next ledger event for `thread_id` after the cursor, or `None` when
    /// `deadline` passes. Each item has the shape the firehose's `event` frame
    /// had where the run reads it: `{seq, message_id, ts, event}`.
    async fn next_event(&mut self, thread_id: &str, deadline: Instant) -> Option<Value> {
        loop {
            if let Some(e) = self.events.pop_front() {
                return Some(e);
            }
            let page = self
                .request_ok(
                    &format!("threads/{thread_id}/history"),
                    json!({ "after_seq": self.cursor, "limit": 500 }),
                )
                .await;
            for e in page["events"].as_array().cloned().unwrap_or_default() {
                if let Some(s) = e["seq"].as_u64() {
                    self.cursor = self.cursor.max(s);
                }
                self.events.push_back(e);
            }
            if !self.events.is_empty() {
                continue;
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            tokio::time::sleep(left.min(POLL)).await;
        }
    }
}

/// How often the run re-reads the ledger while nothing new has landed.
const POLL: Duration = Duration::from_millis(150);

fn lock_path(explicit: Option<String>) -> std::path::PathBuf {
    if let Some(p) = explicit {
        return std::path::PathBuf::from(p);
    }
    let home = std::env::var("THREADS_HOME").ok().filter(|s| !s.is_empty());
    match home {
        Some(h) => std::path::PathBuf::from(h).join("stress.lock"),
        None => std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
            .join(".threads/stress.lock"),
    }
}

/// EXACTLY ONE EXECUTION AT A TIME. Any other thread that is busy, retrying,
/// holding queued work or sitting on a parked turn means we do not start:
/// a stress run has to be the only thing consuming the operator's usage.
async fn assert_exclusive(c: &mut Client, target: Option<&str>) -> Vec<String> {
    let list = c.request_ok("threads/list", json!({ "limit": 500 })).await;
    let mut active = Vec::new();
    for t in list["threads"].as_array().cloned().unwrap_or_default() {
        let id = t["thread_id"].as_str().unwrap_or_default().to_string();
        if Some(id.as_str()) == target {
            continue;
        }
        // `state` is the derivation's own word (`STATE_WORDS`: active, parked,
        // retrying, queued, idle), and `queued` IS one of them. `status` is the
        // older three-word rendering that folds `queued` into "idle", which is
        // why this used to ask `threads/{id}/queue` a second time — a read that
        // no longer exists, because the `threads` shape's cell carries it.
        let state = t["state"].as_str().unwrap_or("idle");
        if state != "idle" {
            active.push(format!("{id} is {state}"));
        }
    }
    active
}

#[tokio::main]
async fn main() {
    let a = parse_args();
    let url = a
        .url
        .clone()
        .or_else(|| std::env::var("THREADS_STRESS_URL").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:3653/ws".to_string());
    let secret = a
        .secret
        .clone()
        .or_else(|| std::env::var("THREADS_SECRET").ok().filter(|s| !s.is_empty()));

    let _lock = match Lock::take(lock_path(a.lock.clone())) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            println!(
                "{}",
                json!({"outcome": "refused", "reason": "another stress run holds the lock"})
            );
            die(
                3,
                "REFUSED: another stress run holds the lock (remove it by hand if that run is dead)",
            )
        }
        Err(e) => die(1, &format!("taking the stress lock: {e}")),
    };

    let started = Instant::now();
    let mut c = Client::connect(&url, secret.as_deref()).await;

    // ── exclusivity ──────────────────────────────────────────────────────
    let active = assert_exclusive(&mut c, a.thread.as_deref()).await;
    if !active.is_empty() && !a.allow_active {
        println!(
            "{}",
            json!({"outcome": "refused", "reason": "other threads are active", "active": active})
        );
        die(
            3,
            &format!("REFUSED: {} — nothing else may run during a stress test (--allow-active overrides)", active.join("; ")),
        );
    }

    // ── the thread under test ────────────────────────────────────────────
    let thread_id = match &a.thread {
        Some(t) => t.clone(),
        None => {
            let cwd = a
                .cwd
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap().display().to_string());
            let settings = json!({
                "cwd": cwd,
                "model": a.model.clone().unwrap_or_else(|| "sonnet".into()),
                "metadata": {"stress_test": true},
            });
            let p = c
                .request_ok("threads/create", json!({ "settings": settings }))
                .await;
            p["thread_id"].as_str().unwrap().to_string()
        }
    };

    // ── capture what we are about to change, so we can put it back ───────
    let before = c
        .request(&format!("threads/{}/budget_get", thread_id), json!({ }))
        .await;
    let prior_thresholds = before["payload"]["thresholds"].clone();
    let prior_budget = match &a.key {
        Some(k) => c.request(&format!("budgets/{}/get", k), json!({ })).await,
        None => Value::Null,
    };
    let key_existed = prior_budget["type"] == json!("success");
    // Q-34: a key that is DISABLED denies every turn, so a run against one
    // would measure nothing but the disable gate. Enable it for the run and put
    // the deny switch back exactly as it was found.
    let prior_disabled = key_existed && prior_budget["payload"]["disabled"] == json!(true);

    // ── apply the value under test ───────────────────────────────────────
    let mut applied = serde_json::Map::new();
    if let Some(key) = &a.key {
        let mut params = json!({});
        for (name, v) in [
            ("session_pct", a.session_pct),
            ("weekly_pct", a.weekly_pct),
            ("fable_pct", a.fable_pct),
            ("cash_usd", a.cash_usd),
        ] {
            if let Some(v) = v {
                params[name] = json!(v);
            }
        }
        if let Some(t) = &a.on_trip {
            params["on_trip"] = json!(t);
        }
        if let Some(accts) = &a.accounts {
            params["accounts"] = json!(accts);
        } else if !key_existed {
            die(
                2,
                "--accounts is required when creating a budget key (the crate never \
                 defaults a budget to every account)",
            );
        }
        let set = c.request_ok(&format!("budgets/{}/set", key), params.clone()).await;
        applied.insert("budget".into(), params);
        applied.insert("budget_state".into(), set);
        if prior_disabled {
            c.request_ok(&format!("budgets/{}/enable", key), json!({})).await;
            applied.insert("re_enabled_for_the_run".into(), json!(true));
        }
        c.request_ok(
            &format!("threads/{}/budget_assign", thread_id),
            json!({"key": key}),
        )
        .await;
    }
    let want_thresholds = a.th_session.is_some()
        || a.th_weekly.is_some()
        || a.th_fable.is_some()
        || a.th_cash.is_some()
        || a.clear_thresholds;
    if want_thresholds {
        let mut th = if a.clear_thresholds {
            json!({})
        } else {
            prior_thresholds.clone()
        };
        if !th.is_object() {
            th = json!({});
        }
        for (name, v) in [
            ("session", a.th_session),
            ("weekly", a.th_weekly),
            ("fable", a.th_fable),
            ("cash", a.th_cash),
        ] {
            if let Some(v) = v {
                th[name] = json!(v);
            }
        }
        c.request_ok(
            &format!("threads/{}/update", thread_id),
            json!({"patch": {"thresholds": th}}),
        )
        .await;
        applied.insert("thresholds".into(), th);
    }

    let gauge = c.request_ok("gauge/status", json!({})).await;

    if a.dry_run {
        println!(
            "{}",
            json!({
                "outcome": "dry_run", "thread_id": thread_id,
                "applied": Value::Object(applied), "gauge": gauge,
                "exclusive": active.is_empty(),
            })
        );
        return;
    }

    // ── the single execution ─────────────────────────────────────────────
    c.watch_from_tail(&thread_id).await;
    let text = a
        .text
        .clone()
        .unwrap_or_else(|| "stress test: reply with the single word ok".to_string());
    c.request_ok(
        &format!("threads/{}/send", thread_id),
        json!({"text": text}),
    )
    .await;

    let hard_deadline = Instant::now() + Duration::from_secs(a.timeout_secs);
    let mut seen: Vec<Value> = Vec::new();
    let mut started_turn = false;
    let mut park: Option<Value> = None;
    let mut tripped: Option<Value> = None;
    let mut killed = false;
    let mut failed: Option<Value> = None;
    let mut exit_code: Option<i64> = None;
    let mut budgets_failed: Option<Value> = None;
    // A park is only conclusive once it has HELD: a threshold can clear a
    // moment later and the turn goes out by itself.
    let mut settle_deadline: Option<Instant> = None;
    // The execution is over, but a KILLED turn parks its prompt immediately
    // afterwards — keep reading briefly so the report can prove it.
    let mut ended = false;

    loop {
        let deadline = match settle_deadline {
            Some(s) => s.min(hard_deadline),
            None => hard_deadline,
        };
        let Some(f) = c.next_event(&thread_id, deadline).await else {
            if settle_deadline.is_some() && !started_turn {
                break; // the park held
            }
            break; // hard timeout
        };
        let kind = f["event"]["kind"].as_str().unwrap_or("").to_string();
        seen.push(json!({"kind": kind, "ts": f["ts"], "event": f["event"]}));
        match kind.as_str() {
            "turn_parked" => {
                park = Some(f["event"].clone());
                if ended {
                    break; // the post-kill park: everything is known
                }
                settle_deadline = Some(Instant::now() + Duration::from_secs(a.settle_secs));
            }
            "turn_resumed" => {
                park = None;
                settle_deadline = None;
            }
            "turn_started" => {
                started_turn = true;
                settle_deadline = None;
            }
            "budget_tripped" => tripped = Some(f["event"].clone()),
            "turn_killed" => killed = true,
            "budgets_failed" => budgets_failed = Some(f["event"].clone()),
            "turn_failed" => {
                failed = Some(f["event"].clone());
                break;
            }
            "turn_ended" => {
                exit_code = f["event"]["exit_code"].as_i64();
                ended = true;
                if !killed {
                    break;
                }
                // A KILLED turn parks its prompt a beat later (CON-4); drain
                // briefly so the report can prove nothing was dropped.
                settle_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
            _ => {}
        }
    }

    let outcome = if failed.is_some() {
        "failed"
    } else if killed {
        "killed"
    } else if exit_code.is_some() {
        if tripped.is_some() {
            "tripped" // it tripped a budget and was allowed to finish
        } else {
            "finished"
        }
    } else if let Some(p) = &park {
        match p["cause"].as_str().unwrap_or("") {
            // A GATE said no before anything ran. `disabled` and `unknown_key`
            // (Q-34) are gates too: they deny without reading a single gauge.
            "threshold" | "scope" | "continuity" | "disabled" | "unknown_key" => "gated",
            _ => "parked",
        }
    } else {
        "timeout"
    };

    // ── put the configuration back ───────────────────────────────────────
    let mut restored = Value::Null;
    if !a.no_restore {
        if let Some(key) = &a.key {
            if key_existed {
                let cfg = &prior_budget["payload"]["config"];
                let mut params = json!({
                    "session_pct": cfg["session_pct"].clone(),
                    "weekly_pct": cfg["weekly_pct"].clone(),
                    "fable_pct": cfg["fable_pct"].clone(),
                    "cash_usd": cfg["cash_usd"].clone(),
                });
                if let Some(t) = cfg["on_trip"].as_str() {
                    params["on_trip"] = json!(t);
                }
                if cfg["accounts"].is_array() {
                    params["accounts"] = cfg["accounts"].clone();
                }
                c.request(&format!("budgets/{}/set", key), params).await;
                if prior_disabled {
                    // It was disabled when we found it; it is disabled again now.
                    c.request(&format!("budgets/{}/disable", key), json!({})).await;
                }
            } else {
                // Q-34: keys are never deleted. Unpoint the thread FIRST (so it
                // is not left aimed at a key that denies), then disable the key
                // the driver minted — inert, and still queryable.
                c.request(
                    &format!("threads/{}/budget_assign", thread_id),
                    json!({"key": Value::Null}),
                )
                .await;
                c.request(&format!("budgets/{}/disable", key), json!({ })).await;
            }
        }
        if want_thresholds {
            let th = if prior_thresholds.is_object() {
                prior_thresholds.clone()
            } else {
                json!({})
            };
            c.request(
                &format!("threads/{}/update", thread_id),
                json!({"patch": {"thresholds": th}}),
            )
            .await;
        }
        restored = json!(true);
    }

    let report = json!({
        "outcome": outcome,
        "thread_id": thread_id,
        "text": text,
        "applied": Value::Object(applied),
        "gauge_before": gauge,
        "turn_started": started_turn,
        "tripped": tripped,
        "killed": killed,
        "parked": park,
        "budgets_failed": budgets_failed,
        "turn_failed": failed,
        "exit_code": exit_code,
        "restored": restored,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "events": seen,
    });
    println!("{report}");
    eprintln!(
        "threads-stress: {outcome} (thread {thread_id}, {} ms)",
        started.elapsed().as_millis()
    );
    if outcome == "timeout" {
        // Same rule as `die`: exit without leaving the lock behind.
        release_lock();
        std::process::exit(4);
    }
}
