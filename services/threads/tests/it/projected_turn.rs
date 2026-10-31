//! **THE PROJECTED TURN, END TO END** — R4.S4a's proofs, against a daemon
//! bound to `127.0.0.1:0` and the fake claude fixture's `projected` mode.
//!
//! What is proved here, each test naming its forced red:
//!
//! * `an_unflipped_thread_runs_exactly_as_today` — with the switch at its
//!   default a turn leaves NO ledger, NO rendered file and today's exact event
//!   sequence. Forced red by flipping `Projection::default()`.
//! * `a_flipped_thread_renders_spawns_reconciles_and_round_trips` — one thread
//!   flipped over `threads.update` runs render → spawn → exit → reconcile
//!   twice; the ledger holds `projected`, the delivered prompt, the wire, and
//!   `reconciled` with the prompt confirmed and the losses named; the archive
//!   parser's reading of the native file agrees with the live ledger at
//!   distance zero; the second render is a walkable chain the runtime resumed;
//!   placement passes clean and refuses once the file moves.
//! * `a_render_the_thread_does_not_allow_parks_the_turn` — a ledger whose
//!   render would lose a prompt's text parks the turn with cause `projection`
//!   and the loss named, ONCE (no re-ask while its inputs are unchanged);
//!   allowing the loss by name lets the parked turn run.
//!
//! Every test drives the FAKE claude (`tests/fixtures/fake-claude`), gets an
//! explicit `data_dir` under a tempdir, and points `THREADS_CLAUDE_HOME` at
//! scratch — nothing here can read or write the operator's own login or the
//! live ledger. `THREADS_CHASSIS_PORT` is unset in-process and the daemon binds
//! `:0`; nothing dials 3651, 3652 or 3653.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use threads::ledger::archive::{ingest_thread, prompts_from_native};
use threads::ledger::claude_code::{chain_break, read_native, render_lines, RenderEnv};
use threads::ledger::turn::{ledger_path, native_path, PlacementRefused};
use threads::ledger::{Kind, Ledger};
use threads::settings::{Projection, SettingsPatch, ThreadSettings};
use threads::{build_manager, Config, ThreadManager};

fn fake_claude() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(first_exec::warmed("tests/fixtures/fake-claude"))
        .to_string_lossy()
        .to_string()
}

fn set_env() {
    std::env::set_var("THREADS_CLAUDE_BIN", fake_claude());
    std::env::set_var("THREADS_HEARTBEAT_MS", "60");
    std::env::set_var("THREADS_SUBAGENT_IDLE_MS", "200");
    std::env::set_var("THREADS_TAIL_POLL_MS", "20");
    std::env::set_var("THREADS_RESUME_TICK_MS", "150");
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
    // NEVER the operator's own `~/.claude`.
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
    // The daemon under test binds `:0` below; the dead-port export must not
    // reach `Config` as a port to prefer.
    std::env::remove_var("THREADS_CHASSIS_PORT");
}

const ACCOUNT: &str = "aa-owned";

fn account_dir(dir: &Path) -> PathBuf {
    dir.join(ACCOUNT)
}

fn manager_at(dir: &Path) -> ThreadManager {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(ACCOUNT.to_string(), account_dir(dir).to_string_lossy().to_string());
    build_manager(Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap()
}

/// A daemon on an ephemeral port: the manager, served by the chassis on
/// `127.0.0.1:0`. The handle is leaked so the socket lives as long as the test.
async fn start(dir: &Path) -> (ThreadManager, SocketAddr) {
    let m = manager_at(dir);
    let (addr, h) = threads::chassis::serve(std::sync::Arc::new(m.clone()), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    std::mem::forget(h);
    (m, addr)
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..600 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

fn kinds(m: &ThreadManager, id: &str) -> Vec<String> {
    m.store
        .list_events(id, 0, 100_000, None)
        .unwrap()
        .iter()
        .map(|e| e.event.kind().to_string())
        .collect()
}

fn count_kind(m: &ThreadManager, id: &str, kind: &str) -> usize {
    kinds(m, id).iter().filter(|k| k.as_str() == kind).count()
}

fn ledger_kinds(l: &Ledger) -> Vec<String> {
    l.events()
        .iter()
        .map(|e| match e.kind {
            Kind::System => format!("system/{}", e.body["what"].as_str().unwrap_or("?")),
            k => k.word().to_string(),
        })
        .collect()
}

fn thread_cwd(dir: &Path) -> PathBuf {
    dir.join("work").join("x")
}

// ═════════════════════════════════════════════════════════════════════════
// THE ROUND-TRIP DISTANCE — field by field, not a uuid set
// ═════════════════════════════════════════════════════════════════════════

/// The keys of a conversation line the runtime's loader reads back on
/// `--resume`: what the model is shown (`message.role`, `message.content`),
/// how the API turn is identified (`message.id`, `message.model`,
/// `requestId`) and how the line is classed (`type`, `isMeta`). Chain fields
/// (`parentUuid`, `timestamp`, `cwd`, …) are the render's own and are checked
/// by `chain_break`, not here.
fn loader_view(l: &Value) -> Value {
    serde_json::json!({
        "type": l["type"],
        "isMeta": l.get("isMeta").cloned().unwrap_or(Value::Bool(false)),
        "role": l["message"]["role"],
        "content": l["message"]["content"],
        "message_id": l["message"]["id"],
        "model": l["message"]["model"],
        "requestId": l["requestId"],
    })
}

fn native_lines(native: &Path) -> Vec<Value> {
    std::fs::read_to_string(native)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A prompt's text, whichever spelling the line uses (a bare string, or one
/// text block).
fn prompt_text(l: &Value) -> Option<String> {
    match &l["message"]["content"] {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("")),
        _ => None,
    }
}

/// **THE ROUND-TRIP DISTANCE.** The render of `ledger` against the file the
/// runtime actually wrote, as a list of differences (empty is distance 0):
///
/// * every assistant line, matched by uuid, must agree on every key in
///   `loader_view` — a render that keeps the uuid and empties the body is a
///   difference, which the uuid-set metric this replaces could not see;
/// * the operator's prompts (non-meta user lines carrying text) must agree in
///   order and in text — the render files them under the daemon's ids, so
///   they are compared by position, not by uuid.
fn render_distance_lines(rendered: &[Value], real: &[Value]) -> Vec<String> {
    let mut diffs = Vec::new();
    let by_uuid = |ls: &[Value]| -> std::collections::BTreeMap<String, Value> {
        ls.iter()
            .filter(|l| l["type"] == "assistant")
            .map(|l| (l["uuid"].as_str().unwrap_or("").to_string(), l.clone()))
            .collect()
    };
    let (r, n) = (by_uuid(rendered), by_uuid(real));
    for (id, nl) in &n {
        match r.get(id) {
            None => diffs.push(format!("assistant {id}: in the runtime's file, missing from the render")),
            Some(rl) => {
                let (rv, nv) = (loader_view(rl), loader_view(nl));
                for k in nv.as_object().unwrap().keys() {
                    if rv[k] != nv[k] {
                        diffs.push(format!("assistant {id}: `{k}` rendered {} but real {}", rv[k], nv[k]));
                    }
                }
            }
        }
    }
    for id in r.keys().filter(|id| !n.contains_key(*id)) {
        diffs.push(format!("assistant {id}: rendered, not in the runtime's file"));
    }
    let prompts = |ls: &[Value]| -> Vec<String> {
        ls.iter()
            .filter(|l| l["type"] == "user" && l.get("isMeta") != Some(&Value::Bool(true)))
            .filter(|l| l["message"]["content"].is_string() || l["message"]["content"][0]["type"] == "text")
            .filter_map(prompt_text)
            .collect()
    };
    let (rp, np) = (prompts(rendered), prompts(real));
    if rp != np {
        diffs.push(format!("prompts differ: rendered {rp:?} but real {np:?}"));
    }
    diffs
}

fn render_distance(ledger: &Ledger, env: &RenderEnv, native: &Path) -> Vec<String> {
    let (lines, _) = render_lines(ledger, env);
    render_distance_lines(&lines, &native_lines(native))
}

/// The instrument's sentinel: the same comparison, over the same two inputs,
/// after the render is broken the way the inspector broke it (the assistant
/// body emptied, uuid kept) and after a prompt's text is altered. Each must
/// measure non-zero, or `render_distance == []` above proves nothing.
fn instrument_can_see_a_dropped_body(ledger: &Ledger, env: &RenderEnv, native: &Path) {
    let (lines, _) = render_lines(ledger, env);
    let real = native_lines(native);
    let mut emptied = lines.clone();
    for l in emptied.iter_mut().filter(|l| l["type"] == "assistant") {
        l["message"]["content"] = serde_json::json!([]);
    }
    let d = render_distance_lines(&emptied, &real);
    assert!(
        d.iter().any(|x| x.contains("`content`")),
        "the distance sees an assistant body the render dropped: {d:?}"
    );
    let mut reworded = lines.clone();
    for l in reworded.iter_mut().filter(|l| l["type"] == "user") {
        l["message"]["content"] = serde_json::json!("not what he typed");
    }
    let d = render_distance_lines(&reworded, &real);
    assert!(d.iter().any(|x| x.starts_with("prompts differ")), "the distance sees a prompt the render changed: {d:?}");
}

// ═════════════════════════════════════════════════════════════════════════
// 1. THE DEFAULT IS TODAY
// ═════════════════════════════════════════════════════════════════════════

/// **THE SWITCH DEFAULTS OFF.** Its own test, so that the behavioural claim
/// below reds on its own words rather than behind this precondition.
///
/// **THE MUTATION.** In `settings.rs`, move `#[default]` from
/// `Projection::NativeOwned` to `Projection::Projected`.
///
/// Forced red:
/// ```text
/// the switch defaults OFF on trunk
/// ```
#[test]
fn the_switch_defaults_off_on_trunk() {
    assert_eq!(Projection::default(), Projection::NativeOwned, "the switch defaults OFF on trunk");
    let s = ThreadSettings::new("/w".to_string(), "fable".to_string());
    assert_eq!(s.projection, Projection::NativeOwned, "…and a fresh settings blob carries that default");
    assert!(s.projection_allow_loss.is_empty(), "…and allows no loss by name");
    // A stored blob that predates the field reads as the default: the whole
    // backfill, with no migration.
    let old_blob = r#"{"cwd":"/w","model":"fable","settings_json":null}"#;
    let back = ThreadSettings::from_stored(old_blob).unwrap();
    assert_eq!(back.projection, Projection::NativeOwned, "a blob written before the field reads as native_owned");
}

/// **AN UNFLIPPED THREAD LEAVES NO TRACE OF THE PROJECTION.** No ledger file,
/// no file rendered under the cwd's slug (the fake files its own under
/// `-fake-slug`, as it always did), and the turn's event sequence is the one
/// every turn has produced before this step existed.
///
/// **THE MUTATION.** In `settings.rs`, move `#[default]` from
/// `Projection::NativeOwned` to `Projection::Projected`.
///
/// Forced red:
/// ```text
/// an unflipped thread writes no ledger
/// ```
#[tokio::test]
async fn an_unflipped_thread_runs_exactly_as_today() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = thread_cwd(tmp.path());
    std::fs::create_dir_all(&cwd).unwrap();
    let (m, addr) = start(tmp.path()).await;
    assert_eq!(addr.port() != 0, true, "the daemon bound an ephemeral port");
    let s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    let id = m.create_thread(None, s).unwrap();
    m.send(&id, "hello".into(), Default::default(), Value::Null, false).unwrap();
    assert!(eventually(|| count_kind(&m, &id, "turn_ended") == 1).await, "the turn ran: {:?}", kinds(&m, &id));

    assert!(
        !ledger_path(tmp.path(), &id).exists() && !tmp.path().join("ledgers").exists(),
        "an unflipped thread writes no ledger"
    );
    let home = PathBuf::from(std::env::var("THREADS_CLAUDE_HOME").unwrap());
    let rendered = native_path(
        Some(account_dir(tmp.path()).to_str().unwrap()),
        &home,
        cwd.to_str().unwrap(),
        &id,
    );
    assert!(!rendered.exists(), "nothing was rendered under the cwd's slug: {}", rendered.display());
    let got = kinds(&m, &id);
    let turn: Vec<&str> = got
        .iter()
        .skip_while(|k| k.as_str() != "turn_started")
        .map(String::as_str)
        .collect();
    assert_eq!(
        turn,
        vec!["turn_started", "system_init", "assistant_text", "message_usage", "result_turn", "turn_ended"],
        "today's event sequence, exactly: {got:?}"
    );
    assert_eq!(count_kind(&m, &id, "turn_parked"), 0);
}

// ═════════════════════════════════════════════════════════════════════════
// 2. THE FLIPPED THREAD, END TO END, TWICE
// ═════════════════════════════════════════════════════════════════════════

/// **RENDER → SPAWN → EXIT → RECONCILE, AND THE ARCHIVE PARSER AGREES.**
///
/// **THE MUTATION.** In `settings.rs`, delete `set!(projection)` from
/// `ThreadSettings::apply` — the flip over `threads.update` silently does
/// nothing and the first assertion goes red. (A second, deeper mutation:
/// in `ledger/dirty.rs::reconcile`, never confirm a prompt — `prompts_confirmed`
/// reads 0 and the round-trip's prompt check goes red.)
///
/// Forced red:
/// ```text
/// the projection switch is patchable over threads.update
/// ```
#[tokio::test]
async fn a_flipped_thread_renders_spawns_reconciles_and_round_trips() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = thread_cwd(tmp.path());
    std::fs::create_dir_all(&cwd).unwrap();
    let (m, _addr) = start(tmp.path()).await;
    let home = PathBuf::from(std::env::var("THREADS_CLAUDE_HOME").unwrap());
    let s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    let id = m.create_thread(None, s).unwrap();

    // ── Flip it, over the one settings door. ──
    m.update_settings(
        &id,
        SettingsPatch {
            projection: Some(Projection::Projected),
            ..Default::default()
        },
    )
    .unwrap();
    let stored = m.store.get_thread(&id).unwrap().unwrap().settings_json;
    assert_eq!(
        ThreadSettings::from_stored(&stored).unwrap().projection,
        Projection::Projected,
        "the projection switch is patchable over threads.update: {stored}"
    );

    // ── Turn one. ──
    m.send(&id, "projected one".into(), Default::default(), Value::Null, false).unwrap();
    let lp = ledger_path(tmp.path(), &id);
    assert!(
        eventually(|| std::fs::read_to_string(&lp).map(|t| t.contains("\"reconciled\"")).unwrap_or(false)).await,
        "the turn ran and reconciled: {:?}",
        kinds(&m, &id)
    );
    assert_eq!(count_kind(&m, &id, "turn_parked"), 0, "nothing parked: {:?}", kinds(&m, &id));
    assert_eq!(count_kind(&m, &id, "turn_ended"), 1);
    // The `events` table is still written beside the ledger.
    assert!(count_kind(&m, &id, "assistant_text") >= 1);

    let native = native_path(Some(account_dir(tmp.path()).to_str().unwrap()), &home, cwd.to_str().unwrap(), &id);
    assert!(native.is_file(), "the runtime wrote under the cwd's slug: {}", native.display());
    let ledger = Ledger::read_jsonl(&lp).unwrap();
    let lk = ledger_kinds(&ledger);
    assert_eq!(
        lk,
        vec![
            "turn_started",
            "system/projected",
            "user",
            "system/init",
            "assistant",
            "turn_ended",
            "usage",
            "system/reconciled"
        ],
        "the ledger of one projected turn, in order"
    );
    let projected = &ledger.events()[1];
    assert_eq!(projected.body["detail"]["lines"], 0, "a new thread rendered nothing");
    assert!(projected.turn.is_some(), "the render record carries the turn cookie");
    let reconciled = ledger.events().last().unwrap();
    let d = &reconciled.body["detail"];
    assert_eq!(d["status"], "read");
    assert_eq!(d["prompts_confirmed"], 1, "the runtime's copy of our prompt was confirmed by text: {d}");
    assert_eq!(d["already"], 1, "the assistant line the wire carried was already in the ledger: {d}");
    assert_eq!(d["ingested"], 0);
    assert_eq!(d["losses"]["attachment"], 1);
    assert_eq!(d["losses"]["meta"], 1);
    assert!(d["file_digest"].is_string());

    // ── The round trip: the archive parser's reading of the native file
    // against the live ledger. The census is the instrument. ──
    let real = read_native(&native).unwrap();
    assert_eq!(chain_break(&real.chain), None, "the runtime's file is a walkable chain");
    let prompts = prompts_from_native(&native).unwrap();
    assert_eq!(prompts.len(), 1);
    let delivered = ledger.events().iter().find(|e| e.kind == Kind::User).unwrap().body["text"].as_str().unwrap().to_string();
    assert_eq!(prompts[0].text, delivered, "the prompt as the runtime wrote it IS the prompt as delivered");
    let (from_archive, census) = ingest_thread(&tmp.path().join("turns").join(&id), &prompts).unwrap();
    assert!(census.malformed.is_empty() && census.accounted(), "{}", census.report());
    let conversation = |l: &Ledger| -> Vec<(String, String)> {
        l.events()
            .iter()
            .filter(|e| matches!(e.kind, Kind::Assistant | Kind::Thinking | Kind::ToolUse | Kind::ToolResult | Kind::Compaction))
            .map(|e| (e.id.clone(), e.kind.word().to_string()))
            .collect()
    };
    assert_eq!(
        conversation(&from_archive),
        conversation(&ledger),
        "every conversation event the archive parser reads off the wire is in the live ledger under the same id"
    );
    let env = RenderEnv { cwd: cwd.to_string_lossy().to_string(), version: "x".into(), git_branch: None };
    // The instrument first (doctrine 9): it must be able to come out non-zero.
    instrument_can_see_a_dropped_body(&ledger, &env, &native);
    let d = render_distance(&ledger, &env, &native);
    assert!(d.is_empty(), "round-trip distance zero, field by field: {d:#?}");
    // ── Placement: clean after the reconcile. ──
    assert_eq!(m.placement_check_for_test(&id, "bb-owned"), Ok(()), "a reconciled thread may be placed");

    // ── Turn two: the runtime RESUMES from the render. ──
    m.send(&id, "projected two".into(), Default::default(), Value::Null, false).unwrap();
    assert!(
        eventually(|| std::fs::read_to_string(&lp).map(|t| t.matches("\"reconciled\"").count() == 2).unwrap_or(false)).await,
        "the second turn ran and reconciled: {:?}",
        kinds(&m, &id)
    );
    assert_eq!(count_kind(&m, &id, "turn_parked"), 0);
    let ledger = Ledger::read_jsonl(&lp).unwrap();
    let projections: Vec<&threads::ledger::LedgerEvent> = ledger
        .events()
        .iter()
        .filter(|e| e.kind == Kind::System && e.body["what"] == "projected")
        .collect();
    assert_eq!(projections.len(), 2);
    assert_eq!(
        projections[1].body["detail"]["lines"], 2,
        "the second render carried turn one's prompt and reply: {}",
        projections[1].body
    );
    let real = read_native(&native).unwrap();
    assert_eq!(chain_break(&real.chain), None, "the file the runtime resumed and appended to is a walkable chain");
    let d = render_distance(&ledger, &env, &native);
    assert!(d.is_empty(), "round-trip distance zero after the resumed turn, field by field: {d:#?}");
    let seq: Vec<(&str, bool)> = real.chain.iter().map(|c| (c.kind.as_str(), c.is_meta)).collect();
    assert_eq!(
        seq,
        vec![("user", false), ("assistant", false), ("user", false), ("user", true), ("assistant", false)],
        "render (prompt one under the daemon's id, reply one), then the runtime's turn two"
    );
    assert_eq!(real.other.get("attachment"), Some(&1), "turn two's attachment; turn one's was not rendered (a named loss)");
    let reconciled: Vec<&threads::ledger::LedgerEvent> = ledger
        .events()
        .iter()
        .filter(|e| e.kind == Kind::System && e.body["what"] == "reconciled")
        .collect();
    assert_eq!(reconciled[1].body["detail"]["prompts_confirmed"], 1);
    assert_eq!(reconciled[1].body["detail"]["ingested"], 0, "nothing the daemon did not deliver: {}", reconciled[1].body);

    // ── Placement refuses once the file moves under the ledger. ──
    assert_eq!(m.placement_check_for_test(&id, "bb-owned"), Ok(()));
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&native).unwrap();
        f.write_all(b"{\"type\":\"user\",\"uuid\":\"typed-by-hand\",\"message\":{\"role\":\"user\",\"content\":\"hand\"}}\n")
            .unwrap();
    }
    match m.placement_check_for_test(&id, "bb-owned") {
        Err(PlacementRefused::NativeMoved { path, to_account, .. }) => {
            assert_eq!((path, to_account.as_str()), (native.clone(), "bb-owned"));
        }
        other => panic!("a thread whose native file digest moved is refused at placement: {other:?}"),
    }
}

// ═════════════════════════════════════════════════════════════════════════
// 3. THE REFUSAL REACHES THE LEDGER AS A PARK, TYPED BY NAME
// ═════════════════════════════════════════════════════════════════════════

/// **A LOSS THE THREAD DOES NOT ALLOW PARKS THE TURN; ALLOWING IT BY NAME
/// LETS THE PARKED TURN RUN.** The ledger is seeded with a user event whose
/// text the ledger does not hold — the one loss not ruled accepted.
///
/// **THE MUTATION.** In `runtime.rs::run_once`, replace the
/// `Err(e) => return self.refuse_projection(...)` arm with `Err(_) => {}`.
///
/// Forced red:
/// ```text
/// a render the thread does not allow parks the turn with cause `projection`
/// ```
///
/// **THE SECOND MUTATION.** In `runtime.rs::maybe_dispatch`, delete the
/// `projection_hold` cause gate (the hot loop of inspect finding 1).
///
/// Forced red:
/// ```text
/// a refused projection is parked once and not re-asked while its inputs are unchanged
/// ```
#[tokio::test]
async fn a_render_the_thread_does_not_allow_parks_the_turn() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = thread_cwd(tmp.path());
    std::fs::create_dir_all(&cwd).unwrap();
    let (m, _addr) = start(tmp.path()).await;
    let s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    let id = m.create_thread(None, s).unwrap();
    m.update_settings(
        &id,
        SettingsPatch {
            projection: Some(Projection::Projected),
            ..Default::default()
        },
    )
    .unwrap();
    // Seed the ledger with a prompt whose text is gone.
    let lp = ledger_path(tmp.path(), &id);
    std::fs::create_dir_all(lp.parent().unwrap()).unwrap();
    let mut seeded = Ledger::new(threads::ledger::ThreadMeta::root(id.clone(), "claude_code", "2026-10-10T00:00:00Z"));
    seeded
        .append(
            threads::ledger::Draft::new(Kind::User, "2026-10-10T00:00:01Z", serde_json::json!({"text": null}))
                .with_id("lost-prompt"),
        )
        .unwrap();
    seeded.write_jsonl(&lp).unwrap();

    m.send(&id, "projected after a lost prompt".into(), Default::default(), Value::Null, false).unwrap();
    assert!(
        eventually(|| count_kind(&m, &id, "turn_parked") >= 1).await,
        "a render the thread does not allow parks the turn with cause `projection`: {:?}",
        kinds(&m, &id)
    );
    let parked = m
        .store
        .list_events(&id, 0, 100_000, Some(&["turn_parked".to_string()]))
        .unwrap()
        .remove(0);
    let (cause, reason) = match &parked.event {
        threads::event::ThreadEvent::TurnParked { cause, reason, .. } => (cause.clone(), reason.clone()),
        other => panic!("{other:?}"),
    };
    assert_eq!(cause, "projection");
    assert!(reason.contains("prompt_text_unrecoverable"), "the refusal names the loss: {reason}");
    assert_eq!(count_kind(&m, &id, "turn_ended"), 0, "nothing was spawned");
    // ── THE REFUSAL IS NOT RE-ASKED WHILE NOTHING IT DEPENDS ON MOVES ──
    // (inspect finding 1). Before the hold, the refusal's own `finish_turn`
    // redelivered the parked turn synchronously and it was refused again, in
    // an unbounded loop. The longest period on this path is the placement
    // backstop tick (`THREADS_RESUME_TICK_MS` = 150 ms here); 2 s outlasts it
    // thirteen times, and every tick pass calls `maybe_dispatch` on this
    // thread. The hold itself is 30 s (`THREADS_PROJECTION_HOLD_MS` default),
    // longer than this window, so exactly one park may exist.
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    assert_eq!(
        (count_kind(&m, &id, "turn_parked"), count_kind(&m, &id, "turn_resumed")),
        (1, 0),
        "a refused projection is parked once and not re-asked while its inputs are unchanged: {:?}",
        kinds(&m, &id).iter().filter(|k| k.starts_with("turn_")).collect::<Vec<_>>()
    );
    assert!(m.store.get_parked_turn(&id).unwrap().is_some(), "his prompt is held in the parked row");

    // Allow the loss by name: the parked turn becomes runnable and runs.
    m.update_settings(
        &id,
        SettingsPatch {
            projection_allow_loss: Some(vec!["prompt_text_unrecoverable".into()]),
            ..Default::default()
        },
    )
    .unwrap();
    m.placement_wake("test: loss allowed");
    assert!(
        eventually(|| count_kind(&m, &id, "turn_ended") == 1).await,
        "with the loss allowed, the parked turn ran: {:?}",
        kinds(&m, &id)
    );
    let ledger = Ledger::read_jsonl(&lp).unwrap();
    let projected = ledger
        .events()
        .iter()
        .find(|e| e.kind == Kind::System && e.body["what"] == "projected")
        .unwrap();
    assert_eq!(projected.body["detail"]["losses"]["prompt_text_unrecoverable"], 1, "and the loss is on the record");
    let delivered = ledger
        .events()
        .iter()
        .filter(|e| e.kind == Kind::User)
        .filter_map(|e| e.body["text"].as_str())
        .any(|t| t.contains("projected after a lost prompt"));
    assert!(delivered, "his prompt was kept through the park and delivered when the loss was allowed");
}

// ═════════════════════════════════════════════════════════════════════════
// 4. TWO WRITERS, ONE LEDGER FILE (R4.S4a-land, after R4.S2 merged)
// ═════════════════════════════════════════════════════════════════════════

/// **A FLIPPED PARENT THAT SPAWNS A CHILD MID-TURN KEEPS A READABLE LEDGER.**
/// `ledgers/<parent>.jsonl` has two writers: this turn's `ProjectedTurn`,
/// which holds the ledger in memory from `begin` to `end` and writes through
/// as each wire line lands, and R4.S2's managed runtime (`crate::managed`),
/// which appends the parent's `child{started}` when the parent calls
/// `agents_spawn` — a tool call made DURING the turn. Found by R4.S4a's
/// inspector on the round-22 trial merge: before the fix the turn's next
/// append re-used the `seq` the spawn had just taken, and `read_jsonl` refused
/// the file forever after (`Renumbered`), so the parent's next projected turn
/// parked `LedgerUnreadable` and `Managed::load` errored on the same file.
///
/// The turn is held open after `init` (the fake's `release=` hold), so the
/// spawn lands strictly between two of the turn's own appends — no timing
/// window, the interleaving is forced.
///
/// Opposition (doctrine 2): the file must be readable AND hold BOTH writers'
/// events — the child edge the spawn wrote and the turn's own assistant reply
/// and reconcile — so a fix that drops either writer's events fails, and the
/// parent's next projected turn must run from it.
///
/// **THE MUTATION.** In `ledger/turn.rs::ProjectedTurn::persist`, append
/// `events[persisted..]` straight to the file — no write lock, no re-read of
/// the tail (the pre-land shape).
///
/// Forced red: see an internal report —
/// ```text
/// the parent's ledger is readable after a mid-turn spawn by the managed runtime: Renumbered { … }
/// ```
#[tokio::test]
async fn a_flipped_parent_that_spawns_mid_turn_keeps_a_readable_ledger() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = thread_cwd(tmp.path());
    std::fs::create_dir_all(&cwd).unwrap();
    let (m, _addr) = start(tmp.path()).await;
    let s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    let id = m.create_thread(None, s).unwrap();
    m.update_settings(
        &id,
        SettingsPatch {
            projection: Some(Projection::Projected),
            ..Default::default()
        },
    )
    .unwrap();

    let release = tmp.path().join("release-the-parent-turn");
    m.send(
        &id,
        format!("projected spawner release={} ", release.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();
    let lp = ledger_path(tmp.path(), &id);
    // The turn is OPEN and has already written through mid-turn: the wire's
    // `init` is on disk, its reply is not.
    assert!(
        eventually(|| std::fs::read_to_string(&lp).map(|t| t.contains("\"init\"")).unwrap_or(false)).await,
        "the held turn wrote its wire `init` through to the ledger: {:?}",
        kinds(&m, &id)
    );
    assert!(
        !std::fs::read_to_string(&lp).unwrap().contains("projected reply"),
        "the turn is still held: no reply in the ledger yet"
    );

    // ── The other writer: the parent spawns a child, mid-turn. ──
    let claims = shapewire::token::Claims {
        thread_id: id.clone(),
        run_id: format!("{id}/run-1"),
        root: id.clone(),
        depth: 0,
        turn: "cookie".into(),
    };
    let reply = threads::agents::call(
        &m,
        &claims,
        "agents_spawn",
        serde_json::json!({"task": "a child's task", "request_id": "mid-turn-1", "notify": "hold"}),
    )
    .await
    .expect("the parent spawns a child mid-turn");
    let child = reply["thread_id"].as_str().unwrap().to_string();

    // ── Release the turn: its reply and reconcile land AFTER the spawn. ──
    std::fs::write(&release, b"go").unwrap();
    assert!(
        eventually(|| std::fs::read_to_string(&lp).map(|t| t.contains("\"reconciled\"")).unwrap_or(false)).await,
        "the parent's turn ran to its reconcile: {:?}",
        kinds(&m, &id)
    );
    let ledger = Ledger::read_jsonl(&lp)
        .unwrap_or_else(|e| panic!("the parent's ledger is readable after a mid-turn spawn by the managed runtime: {e:?}"));
    let started: Vec<String> = ledger
        .events()
        .iter()
        .filter(|e| e.kind == Kind::Child && e.body["phase"] == "started")
        .map(|e| e.body["child_id"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(started, vec![child.clone()], "the spawn's child edge survived the turn's writes");
    let lk = ledger_kinds(&ledger);
    let at = |k: &str| lk.iter().position(|x| x == k).unwrap_or_else(|| panic!("{k} in {lk:?}"));
    assert!(
        at("system/init") < at("child") && at("child") < at("assistant"),
        "the edge sits where it happened, between the turn's init and its reply: {lk:?}"
    );
    assert_eq!(lk.last().map(String::as_str), Some("system/reconciled"), "the turn's own events survived: {lk:?}");
    assert!(
        m.managed().load(&id).is_ok(),
        "the managed runtime reads the same file back"
    );

    // ── The parent's NEXT projected turn renders from it. ──
    m.send(&id, "projected after the spawn".into(), Default::default(), Value::Null, false).unwrap();
    assert!(
        eventually(|| std::fs::read_to_string(&lp).map(|t| t.matches("\"reconciled\"").count() == 2).unwrap_or(false)).await,
        "the next projected turn ran from the two-writer ledger: {:?}",
        kinds(&m, &id)
    );
    assert_eq!(count_kind(&m, &id, "turn_parked"), 0, "nothing parked: {:?}", kinds(&m, &id));
    let ledger = Ledger::read_jsonl(&lp).expect("still readable after the second turn");
    assert_eq!(
        ledger.events().iter().filter(|e| e.kind == Kind::Child && e.body["phase"] == "started").count(),
        1
    );
}

// ═════════════════════════════════════════════════════════════════════════
// R4.S4b (HOLDS R17): `projection_allow_loss` is validated at the door
// ═════════════════════════════════════════════════════════════════════════

/// **AN ALLOWED-LOSS NAME NO RENDER CAN MEET IS REFUSED WHERE IT IS WRITTEN,
/// AND NOTHING MOVES; A REAL NAME IS ACCEPTED.** Before the validator a typo
/// was stored, allowed nothing, and surfaced later as a `projection` park
/// naming the very loss the operator thought he had allowed.
///
/// **THE MUTATION.** In `runtime.rs::update_settings`, delete the
/// `validate_allow_loss` check (or, opposed, make it refuse every list).
///
/// Forced red:
/// ```text
/// a misspelled allowed loss is refused at update
/// ```
#[tokio::test]
async fn an_unknown_allowed_loss_is_refused_at_the_door_and_a_known_one_is_kept() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = thread_cwd(tmp.path());
    std::fs::create_dir_all(&cwd).unwrap();
    let m = manager_at(tmp.path());
    let s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    let id = m.create_thread(None, s.clone()).unwrap();

    let typo = m.update_settings(
        &id,
        SettingsPatch {
            projection_allow_loss: Some(vec!["prompt_text_unrecoverabel".into()]),
            ..Default::default()
        },
    );
    let err = typo.expect_err("a misspelled allowed loss is refused at update");
    assert!(err.to_string().contains("prompt_text_unrecoverabel"), "the refusal names it: {err}");
    let row = m.store.get_thread(&id).unwrap().unwrap();
    let stored: ThreadSettings = serde_json::from_str(&row.settings_json).unwrap();
    assert!(stored.projection_allow_loss.is_empty(), "nothing was stored: {:?}", stored.projection_allow_loss);

    m.update_settings(
        &id,
        SettingsPatch {
            projection_allow_loss: Some(vec!["prompt_text_unrecoverable".into()]),
            ..Default::default()
        },
    )
    .expect("a loss a render can meet is accepted");
    let row = m.store.get_thread(&id).unwrap().unwrap();
    let stored: ThreadSettings = serde_json::from_str(&row.settings_json).unwrap();
    assert_eq!(stored.projection_allow_loss, vec!["prompt_text_unrecoverable".to_string()]);

    // The create door refuses the same typo.
    let mut bad = s;
    bad.projection_allow_loss = vec!["nope".into()];
    assert!(m.create_thread(None, bad).is_err(), "create refuses an unknown allowed loss too");
}

// ═════════════════════════════════════════════════════════════════════════
// R4.S4b (HOLDS R19): `idle` across the dequeue → running_turns gap
// ═════════════════════════════════════════════════════════════════════════

/// **A TURN THAT IS STARTING IS NOT IDLE.** The turn is HELD in its prologue
/// — after the queue rows were marked delivered, before `claude` is spawned
/// and `running_turns` written — by holding the `ledgers/` write lock its
/// render needs. No timing window: the gap is held open for as long as the
/// test likes.
///
/// Sentinel (doctrine 9): the predicate the old test helper used ("no running
/// row and no waiting row") must read IDLE inside the held gap, or the gap was
/// not reached and the assertion below measures nothing.
///
/// Opposition: once the lock is released the turn runs and ends, and
/// `is_idle` must then say idle — so a constant `false` fails.
///
/// **THE MUTATION.** In `ThreadManager::is_idle`, drop `!d.executing`.
///
/// Forced red:
/// ```text
/// a thread whose turn is starting is not idle
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_is_not_reported_in_the_gap_between_dequeue_and_the_running_row() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = thread_cwd(tmp.path());
    std::fs::create_dir_all(&cwd).unwrap();
    let m = manager_at(tmp.path());
    let s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    let id = m.create_thread(None, s).unwrap();
    m.update_settings(&id, SettingsPatch { projection: Some(Projection::Projected), ..Default::default() })
        .unwrap();
    assert!(m.is_idle(&id).unwrap(), "a fresh thread is idle");

    let lock = m.ledger_write_lock_for_test();
    let guard = lock.lock().unwrap();
    m.send(&id, "held in the prologue".into(), Default::default(), Value::Null, false).unwrap();
    let old_helper = |m: &ThreadManager| {
        m.store.get_running_turn(&id).unwrap().is_none() && m.store.waiting_queue(&id).unwrap().is_empty()
    };
    assert!(
        eventually(|| old_helper(&m) && m.store.get_thread_lock(&id).unwrap().is_some()).await,
        "sentinel: the turn took the lock and dequeued, with no running row (the gap is reached): {:?}",
        kinds(&m, &id)
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(old_helper(&m), "sentinel: still inside the gap, and the old predicate reads idle");
    assert!(!m.is_idle(&id).unwrap(), "a thread whose turn is starting is not idle");

    drop(guard);
    assert!(
        eventually(|| count_kind(&m, &id, "turn_ended") == 1 && m.is_idle(&id).unwrap()).await,
        "released, the turn runs, ends, and the thread is idle: {:?}",
        kinds(&m, &id)
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
