//! **STRUCTURED OUTPUT, PHASE A: THE CORRECTIVE TURN AND THE HONEST RESULT**
//! (R11.J4, R11.J5 — `docs/STRUCTURED-OUTPUT.md` §3.3–§3.5, §5).
//!
//! J4 proves: an `unmet` answer gets ONE corrective turn by default and then
//! `ok`; three daemon lives around the retry still send exactly one; N ≥ 2
//! children retrying at once each get their own. J5 proves: the spec's own
//! `agents_result` table holds (`structured_output` when met, `schema_unmet` +
//! `errors` when not), and a run with no schema has NO `schema_unmet` key —
//! the R4.S2b-inspect constant is gone; `run_ended.structured` and the
//! `<structured_output>` element in today's `<agent_report>`.
//!
//! The model is `tests/fixtures/fake-claude`, steered by the schema's own
//! `description` (`fake:valid`, `fake:always-invalid`, `fake:fix-on-retry`):
//! the corrective prompt is the daemon's words, so only what rides every turn
//! of the question — the schema — can steer it. An invalid answer is shaped as
//! J1's captures show the CLI's "success without value": no
//! `structured_output` on the result line, only the `StructuredOutput`
//! tool_use input left to judge. The verdict (the `structured_output` fact) is
//! R11.J2's; these tests read it back off the STORE the daemon wrote, never
//! off anything the test itself wrote.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shapewire::envelope::codes;
use shapewire::token::Claims;
use threads::agents;
use threads::config::Config;
use threads::managed::{Halt, META_KEY, STRUCTURED_OUTPUT_EVENT};
use threads::{build_manager, ThreadEvent, ThreadManager};

static SERIAL: Mutex<()> = Mutex::new(());

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env(tmp: &Path) {
    std::env::set_var("CAULDRON_HOME", tmp.join("cauldron"));
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_TAIL_POLL_MS", "25");
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var("FAKE_STATE_DIR", tmp.join("fake-state"));
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::remove_var("THREADS_CHASSIS_PORT");
    std::fs::create_dir_all(tmp.join("fake-state")).unwrap();
}

fn config_at(dir: &Path) -> Config {
    let mut map = BTreeMap::new();
    std::fs::create_dir_all(dir.join("aa-owned")).unwrap();
    map.insert("aa-owned".to_string(), dir.join("aa-owned").to_string_lossy().to_string());
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

/// One daemon life: its own runtime, a manager over `dir`, `recover()` run.
/// `before_recover` runs on the manager before `recover()` (a halt point, a
/// backoff), so a kill point can be armed for what boot itself does.
struct Life {
    rt: tokio::runtime::Runtime,
    m: ThreadManager,
}

impl Life {
    fn boot(dir: &Path) -> Life {
        Life::boot_with(dir, |_| {})
    }

    fn boot_with(dir: &Path, before_recover: impl FnOnce(&ThreadManager)) -> Life {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let m = rt.block_on(async {
            threads::clock::testing::ticking();
            let m = build_manager(config_at(dir)).unwrap();
            before_recover(&m);
            m.recover().await;
            m
        });
        Life { rt, m }
    }

    /// **THE KILL.** Every task stops; nothing after this instant runs.
    fn kill(self) {
        let Life { rt, m } = self;
        rt.shutdown_background();
        drop(m);
    }

    fn thread(&self, dir: &Path) -> String {
        let settings = json!({"cwd": dir.to_string_lossy(), "model": "sonnet", "account": {"named": "aa-owned"}});
        let _ctx = self.rt.enter();
        self.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    }

    fn call(&self, caller: &str, tool: &str, args: Value) -> Result<Value, shapewire::envelope::Refusal> {
        let claims = Claims {
            thread_id: caller.to_string(),
            run_id: format!("{caller}/run-1"),
            root: caller.to_string(),
            depth: 0,
            turn: "cookie".into(),
        };
        self.rt.block_on(agents::call(&self.m, &claims, tool, args))
    }

    fn until(&self, secs: u64, mut f: impl FnMut(&ThreadManager) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if f(&self.m) {
                return true;
            }
            self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(25)).await });
        }
        f(&self.m)
    }
}

/// A schema the fake answers per `mode` (and holds a corrective turn on
/// `hold`, when given). `verdict ∈ {merge, block}`: the fake's valid answer is
/// `merge`, its invalid one `maybe`.
fn schema(mode: &str, hold: Option<&Path>) -> Value {
    let mut d = format!("fake:{mode}");
    if let Some(h) = hold {
        d.push_str(&format!(" hold={}", h.display()));
    }
    json!({
        "type": "object",
        "required": ["verdict"],
        "properties": {"verdict": {"enum": ["merge", "block"]}},
        "description": d,
    })
}

fn spawn(l: &Life, caller: &str, rid: &str, extra: Value) -> String {
    let mut args = json!({"task": format!("answer the question ({rid})"), "request_id": rid});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let ok = l
        .call(caller, "agents_spawn", args)
        .unwrap_or_else(|e| panic!("spawn {rid} from {caller}: {} {}", e.code, e.message));
    ok["thread_id"].as_str().unwrap().to_string()
}

fn stored(m: &ThreadManager, id: &str) -> Vec<threads::StoredEvent> {
    m.store.tail_events(id, 100_000).unwrap()
}

/// `(message id, metadata)` of every user message on `id`, in order.
fn user_messages(m: &ThreadManager, id: &str) -> Vec<(u64, Value)> {
    stored(m, id)
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::UserMessage { metadata, .. } => Some((e.message_id.unwrap_or(0), metadata)),
            _ => None,
        })
        .collect()
}

/// The corrective messages the daemon sent `id`: their `schema_retry` records.
fn retries(m: &ThreadManager, id: &str) -> Vec<Value> {
    user_messages(m, id)
        .into_iter()
        .filter(|(_, md)| md["kind"] == "schema_retry")
        .map(|(_, md)| md[META_KEY]["schema_retry"].clone())
        .collect()
}

/// The `structured_output` facts (R11.J2's) on `id`: their `status`es.
fn verdicts(m: &ThreadManager, id: &str) -> Vec<String> {
    stored(m, id)
        .into_iter()
        .filter_map(|e| match e.event {
            ThreadEvent::Custom { event_type, data } if event_type == STRUCTURED_OUTPUT_EVENT => {
                Some(data["status"].as_str().unwrap_or("?").to_string())
            }
            _ => None,
        })
        .collect()
}

fn wakes(m: &ThreadManager, parent: &str) -> Vec<String> {
    user_messages(m, parent)
        .into_iter()
        .filter(|(_, md)| md["origin"] == "agents")
        .map(|(mid, _)| {
            stored(m, parent)
                .into_iter()
                .find_map(|e| match e.event {
                    ThreadEvent::UserMessage { text, .. } if e.message_id == Some(mid) => Some(text),
                    _ => None,
                })
                .unwrap()
        })
        .collect()
}

/// The child's `run_ended` body, read off the child's ledger FILE.
fn run_ended(dir: &Path, child: &str) -> Option<Value> {
    let p = dir.join("ledgers").join(format!("{child}.jsonl"));
    let l = threads::ledger::Ledger::read_jsonl(&p).ok()?;
    l.events()
        .iter()
        .rev()
        .find(|e| e.kind == threads::ledger::Kind::System && e.body["fact"] == "run_ended")
        .map(|e| e.body.clone())
}

fn task_message_id(m: &ThreadManager, child: &str) -> u64 {
    user_messages(m, child).into_iter().find(|(_, md)| md["origin"] == "agents_spawn").expect("the task message").0
}

fn result_of(l: &Life, parent: &str, child: &str) -> Value {
    l.call(parent, "agents_result", json!({"agent": child})).unwrap_or_else(|e| panic!("result: {} {}", e.code, e.message))
}

/// **AN UNMET ANSWER → ONE CORRECTIVE TURN → OK** (R11.J4 proves, row 1; and
/// J5's `agents_result` table, the met row).
///
/// The corrective message is the daemon's, carries the SAME schema (so its turn
/// gets `--json-schema` too — the fake answers validly only when it does), has
/// the deterministic id `schema-retry:<task>:1`, and shows the errors. The
/// parent is woken ONCE, after the retry: the record waited for it.
///
/// **THE MUTATIONS.** Delete the `schema_retry` call in `turn_finished` → no
/// retry, one `unmet` verdict, the result is `schema_unmet` (red at "one
/// corrective turn"). Drop the schema from the corrective message's metadata →
/// its turn has no `--json-schema`, the fake never answers, the second verdict
/// never comes (red at "unmet then ok").
#[test]
fn an_unmet_answer_gets_one_corrective_turn_then_ok() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let s = schema("fix-on-retry", None);
    let child = spawn(&a, &parent, "fix", json!({"json_schema": s}));
    assert!(a.until(30, |m| wakes(m, &parent).len() == 1), "the parent is woken; verdicts {:?}", verdicts(&a.m, &child));
    let task = task_message_id(&a.m, &child);
    assert_eq!(
        retries(&a.m, &child),
        vec![json!({"id": format!("schema-retry:{task}:1"), "of": task, "n": 1})],
        "exactly one corrective turn, deterministic id"
    );
    let (_, md) = user_messages(&a.m, &child).into_iter().find(|(_, md)| md["kind"] == "schema_retry").unwrap();
    assert_eq!(md[META_KEY]["json_schema"], s, "the corrective turn carries the SAME schema");
    let text = stored(&a.m, &child)
        .into_iter()
        .find_map(|e| match e.event {
            ThreadEvent::UserMessage { text, metadata, .. } if metadata["kind"] == "schema_retry" => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(text.contains("did not match the requested schema"), "{text}");
    assert_eq!(verdicts(&a.m, &child), vec!["unmet", "ok"], "unmet then ok");
    // J5: the met row of the spec's table.
    let r = result_of(&a, &parent, &child);
    assert_eq!(r["outcome"], json!("completed"), "{r}");
    assert_eq!(r["structured_output"], json!({"verdict": "merge"}), "{r}");
    assert!(r.get("schema_unmet").is_none(), "met → no schema_unmet key: {r}");
    assert!(r.get("errors").is_none(), "met → no errors key: {r}");
    // J5: run_ended.structured, and the element inside today's <agent_report>.
    let re = run_ended(tmp.path(), &child).expect("run_ended");
    assert_eq!(re["structured"]["status"], json!("ok"), "{re}");
    assert_eq!(re["structured"]["for_message"], json!(task), "{re}");
    assert_eq!(re["structured"]["attempt"], json!(2), "{re}");
    assert_eq!(re["structured"]["value"], json!({"verdict": "merge"}), "{re}");
    let wake = &wakes(&a.m, &parent)[0];
    let el = format!("<structured_output for_message=\"{task}\" status=\"ok\">{{\"verdict\":\"merge\"}}</structured_output>");
    assert!(wake.contains(&el), "the wake carries {el}: {wake}");
    let open = wake.find("<agent_report").unwrap();
    let close = wake.find("</agent_report>").unwrap();
    let at = wake.find(&el).unwrap();
    assert!(open < at && at < close, "the element is INSIDE <agent_report>: {wake}");
    a.kill();
}

/// **NO SCHEMA → NO `schema_unmet` KEY** (R11.J5 proves, row 2). The opposing
/// case of every structured assertion here: a plain child's result, record and
/// report carry none of it. A constant `schema_unmet: true` (the R4.S2b-inspect
/// bug, HEAD before J5) fails here; a constant "never" fails the unmet test.
#[test]
fn a_run_without_a_schema_has_no_schema_unmet_key_anywhere() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let child = spawn(&a, &parent, "plain", json!({}));
    assert!(a.until(30, |m| wakes(m, &parent).len() == 1), "the parent is woken");
    let r = result_of(&a, &parent, &child);
    assert_eq!(r["outcome"], json!("completed"), "{r}");
    for k in ["schema_unmet", "structured_output", "errors"] {
        assert!(r.get(k).is_none(), "no schema → no `{k}`: {r}");
    }
    let re = run_ended(tmp.path(), &child).expect("run_ended");
    assert!(re.get("structured").is_none(), "{re}");
    assert!(!wakes(&a.m, &parent)[0].contains("<structured_output"), "no element without a schema");
    assert!(retries(&a.m, &child).is_empty());
    // The gather row is the same answer (`agents_gather`, its `result`).
    let g = a.call(&parent, "agents_gather", json!({"runs": "children", "until": "all", "poll_s": 5})).unwrap();
    let row = &g["rows"][0]["result"];
    assert!(row.get("schema_unmet").is_none(), "gather row: {g}");
    a.kill();
}

/// **AN ANSWER THAT NEVER MATCHES: `schema_retries` CORRECTIVE TURNS, THEN
/// `schema_unmet`** (R11.J4 default ONE; 0 and 3 when asked; J5's unmet row).
/// Three children side by side make every constant implementation fail: a
/// constant 1 fails the 0 and 3 children, a constant 0 the default.
#[test]
fn a_never_matching_answer_is_schema_unmet_after_its_retries_default_one() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let s = schema("always-invalid", None);
    let c0 = spawn(&a, &parent, "zero", json!({"json_schema": s, "schema_retries": 0}));
    let c1 = spawn(&a, &parent, "default", json!({"json_schema": s}));
    let c3 = spawn(&a, &parent, "three", json!({"json_schema": s, "schema_retries": 3}));
    assert!(a.until(60, |m| wakes(m, &parent).len() == 1), "the parent is woken once, after all three");
    for (c, n) in [(&c0, 0usize), (&c1, 1), (&c3, 3)] {
        assert_eq!(retries(&a.m, c).len(), n, "{c}: {n} corrective turns");
        assert_eq!(verdicts(&a.m, c), vec!["unmet"; n + 1], "{c}");
        let r = result_of(&a, &parent, c);
        assert_eq!(r["outcome"], json!("completed"), "an unmet never fails the run (D3): {r}");
        assert_eq!(r["schema_unmet"], json!(true), "{r}");
        assert!(r["errors"].as_array().is_some_and(|e| !e.is_empty()), "the errors are served: {r}");
        assert!(r.get("structured_output").is_none(), "an invalid value is never served as the answer: {r}");
        let re = run_ended(tmp.path(), c).expect("run_ended");
        assert_eq!(re["structured"]["status"], json!("unmet"), "{re}");
        assert_eq!(re["structured"]["attempt"], json!(n + 1), "{re}");
    }
    let wake = &wakes(&a.m, &parent)[0];
    assert_eq!(wake.matches("status=\"unmet\"").count(), 3, "{wake}");
    assert!(wake.contains("{\"verdict\":\"maybe\"}</structured_output>"), "the invalid value is shown in the element: {wake}");
    a.kill();
}

/// **`schema_retries` IS CHECKED BEFORE THE COMMIT POINT.** Out of range, not
/// an integer, or without a `json_schema` → `malformed`, nothing spawned; 0
/// and 3 are accepted (above).
#[test]
fn schema_retries_out_of_range_or_alone_is_refused_and_spawns_nothing() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let s = schema("valid", None);
    for (args, why) in [
        (json!({"task": "t", "json_schema": s, "schema_retries": 4}), "above 3"),
        (json!({"task": "t", "json_schema": s, "schema_retries": -1}), "negative"),
        (json!({"task": "t", "json_schema": s, "schema_retries": "1"}), "a string"),
        (json!({"task": "t", "schema_retries": 1}), "without json_schema"),
    ] {
        let e = a.call(&parent, "agents_spawn", args).expect_err(why);
        assert_eq!(e.code, codes::MALFORMED, "{why}: {}", e.message);
        assert!(e.message.contains("schema_retries"), "{why}: {}", e.message);
    }
    assert_eq!(a.m.store.list_threads(100).unwrap().len(), 1, "nothing was spawned");
    a.kill();
}

/// **THREE DAEMON LIVES → ONE CORRECTIVE TURN** (R11.J4 proves, row 2).
///
/// * Life 1 dies between the `unmet` verdict and the corrective message
///   (`Halt::BeforeSchemaRetry`; the live retry's backoff is stretched past
///   the life so it cannot finish the job first).
/// * Life 2's boot sends it — and the corrective turn is HELD mid-flight
///   (`hold=`), and life 2 dies with it running.
/// * Life 3 boots with the retry recorded and its turn in flight: it must not
///   send a second one. The hold is released; the turn ends `ok`.
///
/// Evidence is read off the store after life 3: exactly one corrective
/// message, `schema-retry:<task>:1`, and the result is met.
///
/// **THE MUTATIONS.** Drop the `schema_retry` call in `reconcile` → life 2
/// never sends it, the run ends `unmet` with zero retries (red at "life 2
/// sends the corrective turn"). Make `schema_retry` ignore that the newest
/// message is already the retry (decide from the task message instead) →
/// lives 2 and 3 send two (red at "one corrective turn").
#[test]
fn three_daemon_lives_around_the_retry_send_exactly_one() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let hold = tmp.path().join("release-retry");
    let s = schema("fix-on-retry", Some(&hold));

    // Life 1: the verdict lands, the retry does not.
    let a = Life::boot(tmp.path());
    a.m.managed().set_retry_backoff(Duration::from_secs(600), Duration::from_secs(600));
    a.m.managed().halt_after(Some(Halt::BeforeSchemaRetry));
    let parent = a.thread(tmp.path());
    let child = spawn(&a, &parent, "lives", json!({"json_schema": s}));
    assert!(a.until(30, |m| verdicts(m, &child) == vec!["unmet"]), "life 1: the unmet verdict");
    assert!(a.until(5, |m| m.store.get_running_turn(&child).unwrap().is_none()), "life 1: the turn is over");
    // The halt fired (it disarms itself when it does): nothing was sent.
    a.rt.block_on(async { tokio::time::sleep(Duration::from_millis(300)).await });
    assert!(retries(&a.m, &child).is_empty(), "life 1 died before the corrective message");
    a.kill();

    // Life 2: boot reconciliation sends it; its turn is held, and life 2 dies.
    let b = Life::boot(tmp.path());
    assert!(b.until(20, |m| retries(m, &child).len() == 1), "life 2 sends the corrective turn");
    assert!(b.until(20, |m| m.store.get_running_turn(&child).unwrap().is_some()), "life 2: the retry's turn is running");
    assert!(wakes(&b.m, &parent).is_empty(), "the record waits for the retry");
    b.kill();

    // Life 3: the retry is recorded and in flight — it must not be sent again.
    let c = Life::boot(tmp.path());
    std::fs::write(&hold, b"go").unwrap();
    assert!(c.until(40, |m| wakes(m, &parent).len() == 1), "life 3: the parent is woken; verdicts {:?}", verdicts(&c.m, &child));
    let task = task_message_id(&c.m, &child);
    assert_eq!(
        retries(&c.m, &child),
        vec![json!({"id": format!("schema-retry:{task}:1"), "of": task, "n": 1})],
        "one corrective turn across three lives"
    );
    let r = result_of(&c, &parent, &child);
    assert_eq!(r["structured_output"], json!({"verdict": "merge"}), "{r}");
    c.kill();
}

/// **N ≥ 2 CHILDREN RETRYING AT ONCE** (R11.J4 proves, row 3): three children
/// of one parent, all `unmet` first, each gets ONE corrective turn naming ITS
/// OWN task, each ends met, and the parent is woken once with all three.
#[test]
fn three_children_retrying_at_once_each_get_their_own_one() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let s = schema("fix-on-retry", None);
    let kids: Vec<String> = (0..3).map(|i| spawn(&a, &parent, &format!("k{i}"), json!({"json_schema": s}))).collect();
    assert!(a.until(60, |m| wakes(m, &parent).len() == 1), "the parent is woken once");
    for k in &kids {
        let task = task_message_id(&a.m, k);
        assert_eq!(retries(&a.m, k), vec![json!({"id": format!("schema-retry:{task}:1"), "of": task, "n": 1})], "{k}");
        assert_eq!(verdicts(&a.m, k), vec!["unmet", "ok"], "{k}");
        assert_eq!(result_of(&a, &parent, k)["structured_output"], json!({"verdict": "merge"}));
    }
    let wake = &wakes(&a.m, &parent)[0];
    assert_eq!(wake.matches("status=\"ok\"").count(), 3, "{wake}");
    a.kill();
}

/// **THE ELEMENT IS WELL-FORMED WHATEVER THE VALUE HOLDS** (R11.J5, §3.5): a
/// value containing `</agent_report>` cannot close the envelope; quotes in
/// errors cannot close the attribute; above 8 KiB the value is a `ref`.
#[test]
fn the_structured_output_element_escapes_and_refs_large_values() {
    use threads::ledger::spawn::{structured_element, STRUCTURED_INLINE_MAX};
    let ok = structured_element(&json!({"for_message": 3, "status": "ok", "value": {"verdict": "merge"}}));
    assert_eq!(ok, r#"<structured_output for_message="3" status="ok">{"verdict":"merge"}</structured_output>"#);
    let hostile = structured_element(&json!({"for_message": 3, "status": "ok", "value": {"t": "</agent_report><x>&"}}));
    assert!(!hostile.contains("</agent_report>") && !hostile.contains("<x>"), "{hostile}");
    let body = hostile.split_once('>').unwrap().1.rsplit_once("</structured_output>").unwrap().0;
    let back: Value = serde_json::from_str(body).expect("the body is still JSON");
    assert_eq!(back["t"], json!("</agent_report><x>&"), "and it round-trips");
    let unmet = structured_element(&json!({"for_message": 3, "status": "unmet", "errors": ["/verdict: \"maybe\" is not one of"]}));
    assert_eq!(unmet, r#"<structured_output for_message="3" status="unmet" errors="/verdict: &quot;maybe&quot; is not one of"/>"#);
    let big = structured_element(&json!({"for_message": 3, "status": "ok", "event_seq": 41, "value": {"x": "a".repeat(STRUCTURED_INLINE_MAX)}}));
    assert_eq!(big, r#"<structured_output for_message="3" status="ok" ref="body:41"/>"#);
    let small = structured_element(&json!({"for_message": 3, "status": "ok", "event_seq": 41, "value": {"x": "a".repeat(100)}}));
    assert!(small.contains(&"a".repeat(100)) && !small.contains("ref="), "below the bound it is inline");
}

#[path = "support/first_exec.rs"]
mod first_exec;

/// The words `run_structured`'s no-verdict arm serves (R11.J5).
const NO_ANSWER: &str = "the turn that carried the schema ended with no answer to judge";

/// Assert the honest no-verdict `schema_unmet` on every face of a schema child
/// whose turn ended WITHOUT an answer: `agents_result`, `run_ended.structured`,
/// and no corrective turn (there was nothing to correct). `reported` is the
/// outcome the parent's wake names (`failed` / `cancelled`), so each test shows
/// its turn really ended the way it says; `agents_result`'s own vocabulary is
/// `completed | crashed` (`agents_list`'s filter enum), so it reads `crashed`.
fn assert_unmet_no_answer(a: &Life, dir: &Path, parent: &str, child: &str, reported: &str) {
    let task = task_message_id(&a.m, child);
    assert!(verdicts(&a.m, child).is_empty(), "no answer → no verdict fact: {:?}", verdicts(&a.m, child));
    assert!(retries(&a.m, child).is_empty(), "no answer → no corrective turn");
    let wake = &wakes(&a.m, parent)[0];
    assert!(wake.contains(&format!("outcome=\"{reported}\"")), "the turn ended {reported}: {wake}");
    let r = result_of(a, parent, child);
    assert_eq!(r["outcome"], json!("crashed"), "{r}");
    assert_eq!(r["schema_unmet"], json!(true), "a schema run with no answer is NOT silent: {r}");
    assert_eq!(r["errors"], json!([NO_ANSWER]), "{r}");
    assert!(r.get("structured_output").is_none(), "{r}");
    let re = run_ended(dir, child).expect("run_ended");
    assert_eq!(re["structured"]["status"], json!("unmet"), "{re}");
    assert_eq!(re["structured"]["for_message"], json!(task), "{re}");
    assert_eq!(re["structured"]["attempt"], json!(1), "{re}");
    assert_eq!(re["structured"]["errors"], json!([NO_ANSWER]), "{re}");
    assert!(re["structured"].get("value").is_none(), "{re}");
}

/// **A SCHEMA TURN THAT FAILS IS AN HONEST `schema_unmet`, NOT A SILENCE**
/// (R11.J5, r37 inspect finding 1). The child's only turn fails
/// deterministically (`fake:fail`, ladder of one): there is no verdict, so
/// `run_structured` takes its no-verdict arm. The caller must still be able to
/// tell "schema, no answer" from "no schema".
///
/// **THE MUTATION.** `run_structured`'s `None => {` → `None => { return
/// Ok(None);` → neither key on the result, no `structured` on `run_ended`
/// (red at "a schema run with no answer is NOT silent"). Opposed by
/// `a_schema_turn_that_answers_validly_reads_met_on_the_first_turn`.
#[test]
fn a_schema_turn_that_fails_is_schema_unmet_with_no_answer_to_judge() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = {
        let settings = json!({
            "cwd": tmp.path().to_string_lossy(),
            "model": "sonnet",
            "account": {"named": "aa-owned"},
            "retry_policy": {"max_deterministic_attempts": 1},
        });
        let _ctx = a.rt.enter();
        a.m.create_thread(None, serde_json::from_value(settings).unwrap()).unwrap()
    };
    let child = spawn(&a, &parent, "fails", json!({"json_schema": schema("fail", None)}));
    assert!(a.until(30, |m| wakes(m, &parent).len() == 1), "the failed child wakes its parent");
    assert_unmet_no_answer(&a, tmp.path(), &parent, &child, "failed");
    a.kill();
}

/// **A SCHEMA TURN THAT IS CANCELLED IS AN HONEST `schema_unmet`** (R11.J5,
/// r37 inspect finding 1). The child's schema turn is held mid-flight
/// (`fake:stall`, never released) and cancelled; a cut turn has no answer to
/// judge. Same mutation as the failed case, red at the same assertion.
#[test]
fn a_schema_turn_that_is_cancelled_is_schema_unmet_with_no_answer_to_judge() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let never = tmp.path().join("never");
    let child = spawn(&a, &parent, "cut", json!({"json_schema": schema("stall", Some(&never))}));
    assert!(a.until(10, |m| m.store.get_running_turn(&child).unwrap().is_some()), "the schema turn runs");
    a.m.cancel(&child).unwrap();
    assert!(a.until(30, |m| wakes(m, &parent).len() == 1), "the cancelled child wakes its parent");
    assert_unmet_no_answer(&a, tmp.path(), &parent, &child, "cancelled");
    a.kill();
}

/// **THE OPPOSING CASE: A SCHEMA TURN THAT ANSWERS VALIDLY IS MET, FIRST
/// TIME** (R11.J5). One turn, one `ok` verdict, no corrective turn, the value
/// served and no `schema_unmet`. A constant "no verdict → unmet" taken for
/// every run (forcing: `run_structured` ignores `newest_structured_after` and
/// always takes the no-verdict arm) goes red here, as the
/// `return Ok(None)` forcing goes red in the two tests above.
#[test]
fn a_schema_turn_that_answers_validly_reads_met_on_the_first_turn() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    set_env(tmp.path());
    let a = Life::boot(tmp.path());
    let parent = a.thread(tmp.path());
    let child = spawn(&a, &parent, "valid", json!({"json_schema": schema("valid", None)}));
    assert!(a.until(30, |m| wakes(m, &parent).len() == 1), "the parent is woken");
    assert_eq!(verdicts(&a.m, &child), vec!["ok"]);
    assert!(retries(&a.m, &child).is_empty());
    let r = result_of(&a, &parent, &child);
    assert_eq!(r["outcome"], json!("completed"), "{r}");
    assert!(r.get("schema_unmet").is_none(), "a valid answer must not read unmet: {r}");
    assert_eq!(r["structured_output"], json!({"verdict": "merge"}), "{r}");
    let re = run_ended(tmp.path(), &child).expect("run_ended");
    assert_eq!(re["structured"]["status"], json!("ok"), "{re}");
    assert_eq!(re["structured"]["attempt"], json!(1), "{re}");
    a.kill();
}
