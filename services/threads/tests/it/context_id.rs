//! **THE CONTEXT ID — the retrieval-dedup cache key, and the one property it
//! has to have.**
//!
//! The operator's reason, in substance: a thread retrieving from a large
//! knowledge graph must not re-emit the same fields twice, because re-emitting
//! them burns the very context it is trying to preserve. So the consumer wants
//! to remember what it has already shown this model — and for that it needs a
//! key that is **stable for exactly as long as the context window holds and
//! guaranteed to change the moment the window is reset.** After a compaction
//! the thread ledger is lossy, so re-retrieval is correct there, and it needs a
//! new key.
//!
//! Everything in this file is that one sentence, turned into assertions:
//!
//! * `the_context_id_is_the_boundary_and_changes_only_when_the_boundary_does`
//!   is the test that matters. REAL turns through the fake CLI, a REAL
//!   compaction driven by the fixture's `compactturn` mode, and the assertions
//!   are against the bytes the fixture actually received — never against a
//!   string this file computed and then compared with itself.
//! * `the_flag_is_actually_patchable_over_threads_update` exists because of a
//!   known, repeated failure in `ThreadSettings::apply`: its `set!` macro list
//!   has twice shipped MISSING a field, which made that field's doc comment a
//!   lie. Delete `set!(inject_context_id)` and this test goes red.
//! * `the_context_id_alone_is_a_coherent_request_and_renders_a_one_line_block`
//!   REPLACES the two refusal tests that used to live here. They pinned
//!   `ensure_context_id_coherent` — a validator that existed only because the
//!   flag was nested under `inject_context` and `metadata_level`. With one flag
//!   per injected thing and the block DERIVED from its lines, the combination
//!   those tests refused is now ordinary and delivers a one-line block, so the
//!   validator deleted itself and its tests became their own inverse.
//! * `the_context_id_appears_without_waiting_for_a_compaction` pins the INTERIM
//!   mid-context catch-up, which the metadata-as-projection work will delete.
//!
//! Every test drives the FAKE claude fixture (`tests/fixtures/fake-claude`) or
//! no process at all. No real claude turn is ever spawned, every manager gets
//! an explicit `data_dir` under a tempdir so `~/.threads` cannot come into
//! existence, and `THREADS_CLAUDE_HOME` points at scratch so nothing here can
//! read or write the operator's own login.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Local, TimeZone, Utc};
use serde_json::{json, Value};
use threads::context::{context_id, message_metadata_notice, MessageMeta, SentStamp};
use threads::settings::{SettingsPatch, ThreadSettings};
use threads::{build_manager, Config, ThreadManager};

// ═════════════════════════════════════════════════════════════════════════
// HELPERS
// ═════════════════════════════════════════════════════════════════════════

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
}

fn manager_at(dir: &Path) -> ThreadManager {
    // CON-27: the machine `default` account is never a place threads may put a
    // thread, so an end-to-end fixture has to register an account threads OWNS
    // or `create_thread` refuses before the fake claude is ever reached.
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
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

/// A thread that HAS opted into the capability — injection on, the metadata
/// block at its default level, and the new flag raised. Nothing else moves.
fn with_context_id(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::default();
    s.injection.context_id = true;
    s
}

/// Every prompt the fixture has received on this capture path, in order.
fn captured(path: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    // R9.F2: only NUL-TERMINATED records are prompts. The fixture appends each
    // one with `printf '%s\0'`, and a long prompt reaches the file in more than
    // one write(2); a read landing between them used to count the torn head,
    // which has no terminator yet, as a whole prompt (`context_id` red in the
    // full suite, green alone). Everything after the last NUL is still in flight.
    let raw = raw.rsplit_once('\0').map(|(done, _in_flight)| done).unwrap_or("");
    raw.split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
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

/// The `context:` values a delivered prompt actually states, in order. Parsed
/// out of the bytes rather than matched with `contains`, so a test can assert
/// that a prompt names NO context and be believed.
fn context_lines(prompt: &str) -> Vec<String> {
    prompt
        .lines()
        .filter_map(|l| l.trim().strip_prefix("context: "))
        .map(|v| {
            // The glossed form is `<id> (this context window; …)`. The id is
            // the part before the gloss.
            v.split(" (").next().unwrap_or(v).trim().to_string()
        })
        .collect()
}

// ═════════════════════════════════════════════════════════════════════════
// 1. THE TEST THAT MATTERS — real turns, a real compaction
// ═════════════════════════════════════════════════════════════════════════

/// **The id changes at a compaction and is stable across turns within an
/// epoch.**
///
/// Four real deliveries through the fake CLI. The third one compacts for real:
/// the fixture emits the `compact_boundary` line in the shape `stream.rs`
/// parses, the runtime folds `Compacted`, and `update_thread_compaction` moves
/// `last_compaction_message_id` to 3. Nothing in this test writes that column
/// by hand — a synthetic boundary would prove only that `format!` works.
///
/// "Stable within an epoch" is asserted the way the design actually delivers
/// it: the line rides the BOUNDARY post only, so within an epoch no second,
/// different value is ever stated. A thread that re-derived the id per turn
/// from something else — a per-process counter, a session uuid, a turn number —
/// would have to contradict itself somewhere in here, and there is nowhere left
/// for it to do so.
#[tokio::test]
async fn the_context_id_is_the_boundary_and_changes_only_when_the_boundary_does() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cap = tmp.path().join("prompts");
    let m = manager_at(tmp.path());
    let id = m.create_thread(None, with_context_id(tmp.path())).unwrap();

    // ── EPOCH ZERO. Thread initialisation already set the boundary to 0, so
    // the very first delivery carries `.0` — no mint, no first-turn special
    // case, no counter that could be one behind.
    m.send(
        &id,
        format!("one promptfile={}", cap.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| captured(&cap).len() == 1).await, "first turn delivered");
    let first = captured(&cap).remove(0);
    assert_eq!(
        context_lines(&first),
        vec![format!("{id}.0")],
        "the first delivery states the first context, derived from the boundary \
         thread initialisation already recorded:\n{first}"
    );

    // ── STILL EPOCH ZERO. The second turn rides the same window, so the id
    // has not moved — and the policy proves that by stating nothing, exactly
    // as it does for the thread id. The agent's held value is untouched.
    m.send(
        &id,
        format!("two promptfile={}", cap.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| captured(&cap).len() == 2).await, "second turn delivered");
    let second = captured(&cap).remove(1);
    assert!(
        context_lines(&second).is_empty(),
        "a turn inside the same context must not restate the id — and must \
         certainly not state a DIFFERENT one:\n{second}"
    );

    // ── THE REAL COMPACTION. `compactturn` makes the fixture emit the actual
    // `compact_boundary` stream line; the runtime does the rest.
    m.send(
        &id,
        format!("compactturn please promptfile={}", cap.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| captured(&cap).len() == 3).await, "the compacting turn ran");
    let third = captured(&cap).remove(2);
    assert!(
        context_lines(&third).is_empty(),
        "the compacting turn's own prompt was built BEFORE the boundary moved, \
         so it is still inside epoch zero and still says nothing:\n{third}"
    );
    let boundary = || {
        m.store
            .get_thread(&id)
            .unwrap()
            .map(|r| r.last_compaction_message_id)
            .unwrap_or(0)
    };
    assert!(
        eventually(|| boundary() > 0).await,
        "the compaction must actually have moved the boundary, otherwise this \
         test is not about a compaction at all; it is still at {}",
        boundary()
    );
    // WHICH id the boundary landed on is not this test's business and is
    // deliberately not hardcoded: `alloc_message_id` numbers every ledger
    // message, assistant text included, so the boundary is the highest LEDGER
    // id the compaction swallowed and not the count of the operator's own
    // posts. What matters is that the renderer and the durable column agree,
    // and they are written by different code — `update_thread_compaction` off
    // the `Compacted` fold, versus `context::context_id` at prompt assembly.
    let epoch = boundary();

    // ── EPOCH THREE. The window was reset, so the key MUST be different —
    // this is the whole feature. The boundary post after a compaction is the
    // first thing in the new window, so the agent cannot miss it.
    m.send(
        &id,
        format!("four promptfile={}", cap.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| captured(&cap).len() == 4).await, "the post-compaction turn ran");
    let fourth = captured(&cap).remove(3);
    assert_eq!(
        context_lines(&fourth),
        vec![format!("{id}.{epoch}")],
        "the first post after the compaction must carry a NEW context id, and it \
         must be the boundary the compaction recorded ({epoch}):\n{fourth}"
    );
    assert!(
        !fourth.contains(&format!("{id}.0")),
        "the OLD id must be nowhere on this prompt — a consumer that saw both \
         would not know which cache to trust:\n{fourth}"
    );
    // It rides alongside the thread id, at the boundary, as designed.
    assert!(
        fourth.contains(&format!("thread: {id}")),
        "the context id rides the same post the thread id does:\n{fourth}"
    );

    // ── AND THE NEW EPOCH IS ITSELF STABLE. One more turn, same window, and
    // the id neither repeats nor changes.
    m.send(
        &id,
        format!("five promptfile={}", cap.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| captured(&cap).len() == 5).await, "the fifth turn ran");
    let fifth = captured(&cap).remove(4);
    assert!(
        context_lines(&fifth).is_empty(),
        "epoch three is stable too — nothing restates it and nothing moves \
         it:\n{fifth}"
    );
}

/// **A thread that did not opt in sees no `context:` line at all** — the flag
/// defaults to `false`, and that default is what every existing thread and
/// every AUI thread gets.
#[tokio::test]
async fn a_thread_that_did_not_opt_in_is_told_nothing_about_contexts() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cap = tmp.path().join("prompts");
    let m = manager_at(tmp.path());

    // Injection ON, metadata block ON, flag left at its own default.
    let mut s = ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::default();
    assert!(
        !s.injection.context_id,
        "the capability must be opt-in; a default of true would charge every \
         thread a line of context for a key nobody reads"
    );
    let id = m.create_thread(None, s).unwrap();

    m.send(
        &id,
        format!("one promptfile={}", cap.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| captured(&cap).len() == 1).await);
    let first = captured(&cap).remove(0);
    // The block IS there — this is not a thread with metadata switched off.
    assert!(
        first.contains(&format!("thread: {id}")),
        "baseline: the metadata block is being rendered:\n{first}"
    );
    assert!(
        context_lines(&first).is_empty(),
        "…and the context line is absent because nobody asked for it:\n{first}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 2. THE `set!` GUARD — the edit most likely to be forgotten
// ═════════════════════════════════════════════════════════════════════════

/// **The flag is really patchable over `threads.update`, and the proof is read
/// off the STORED BLOB.**
///
/// `ThreadSettings::apply`'s `set!` list has shipped missing a field twice.
/// Both times the field deserialized, defaulted, and could never afterwards be
/// changed — while its doc comment said it could. This test is the forcing
/// function: delete `set!(inject_context_id)` and it goes red.
///
/// It deliberately does NOT trust the `applied` map the method returns, because
/// that map is populated by the same macro: a missing `set!` line makes both
/// the map and the field wrong together, so a test that only read the map would
/// be checking the mutation against itself. The stored `settings_json` is an
/// independent witness.
#[tokio::test]
async fn the_flag_is_actually_patchable_over_threads_update() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = manager_at(tmp.path());
    let id = m
        .create_thread(
            None,
            ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string()),
        )
        .unwrap();

    let stored = |m: &ThreadManager| -> Value {
        let row = m.store.get_thread(&id).unwrap().unwrap();
        serde_json::from_str::<Value>(&row.settings_json).unwrap()
    };
    assert_eq!(
        stored(&m)["inject_context_id"],
        json!(false),
        "baseline: the thread was created without the capability"
    );

    // ON.
    let patch: SettingsPatch =
        serde_json::from_value(json!({"inject_context_id": true})).unwrap();
    let (applied, _) = m.update_settings(&id, patch).unwrap();
    assert_eq!(
        applied["inject_context_id"],
        json!(true),
        "the audit patch must name the field it changed, got {applied}"
    );
    assert_eq!(
        stored(&m)["inject_context_id"],
        json!(true),
        "THE SETTING MUST ACTUALLY HAVE MOVED ON DISK. If this fails and the \
         assertion above passed, `set!(inject_context_id)` is missing from \
         `ThreadSettings::apply` and the field's doc comment is a lie."
    );

    // …and OFF again, so this proves a live switch and not a one-way latch.
    let patch: SettingsPatch =
        serde_json::from_value(json!({"inject_context_id": false})).unwrap();
    m.update_settings(&id, patch).unwrap();
    assert_eq!(
        stored(&m)["inject_context_id"],
        json!(false),
        "the capability must be revocable mid-thread like every other flag"
    );
}

/// **The posture is on the wire, deliberately — no `skip_serializing_if`.**
///
/// A bool with `#[serde(default)]` has no absent-versus-explicit distinction to
/// protect (unlike `budget_key`, whose missing skip is load-bearing for exactly
/// that reason), so a skip would have been SAFE. It is still wrong: all six
/// sibling bools on `ThreadSettings` are written unconditionally, and a blob
/// that omits the field cannot tell a reader whether the thread has the
/// capability off or merely predates it. The cost is one key in a blob AUI does
/// not diff.
#[test]
fn the_stored_blob_states_the_posture_and_leaves_auis_six_keys_alone() {
    let s = ThreadSettings::new("/tmp/x", "fable");
    let blob: Value = serde_json::to_value(&s).unwrap();
    assert_eq!(
        blob["inject_context_id"],
        json!(false),
        "the field is written even at its default: the blob states the posture \
         rather than leaving a reader to guess. Blob: {blob}"
    );

    // AUI's diff (`AUI_OWNED_SETTINGS`, `lib/threads/docs/aui-data-contract.md`
    // §5.2) is a literal per-key `Value` comparison over exactly these six. A
    // new key outside that set cannot put AUI into a patch loop — which is the
    // only reason the wire change above is affordable.
    for key in [
        "model",
        "append_system_prompt",
        "add_dirs",
        "permission_mode",
        "plugin_roots",
        "inject_context",
    ] {
        assert_ne!(
            key, "inject_context_id",
            "the new field must not be one of AUI's six"
        );
        assert!(
            blob.get(key).is_some(),
            "{key} must still round-trip unchanged for AUI's diff"
        );
    }

    // An OLD blob — stored before this field existed — must read back with the
    // capability off. That is the whole backfill: no migration, no script.
    let old = json!({"cwd": "/tmp/x", "model": "fable"});
    let back: ThreadSettings = serde_json::from_value(old).unwrap();
    assert!(
        !back.injection.context_id,
        "every thread stored before this change inherits the default on read"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 3. THE COMPOSITION — there is nothing left to refuse
// ═════════════════════════════════════════════════════════════════════════

/// **THE SUCCESSOR TO `ensure_context_id_coherent`, and the reason it could
/// delete itself.**
///
/// There used to be a validator at both settings doors because the context id
/// was nested under `inject_context` and `metadata_level`, so a thread could
/// store the flag and render nothing. The operator's ruling: *"ensuring it's
/// coherent sounds like a code thing, not a setting thing. It should always be
/// coherent."* A refusal is an admission that the type permits a meaningless
/// state; the fix is to make it unrepresentable.
///
/// It is unrepresentable because the per-message block is **derived** — it is
/// rendered iff at least one of its lines is on — so "the context id and nothing
/// else" is a perfectly ordinary request that yields a one-line block. This test
/// is that sentence: the exact combination that used to be REFUSED at two doors
/// is now ACCEPTED at both and actually delivers the line.
///
/// Force it red by reintroducing any coherence check: the `create_thread` or
/// `update_settings` call below starts returning `Err`.
#[tokio::test]
async fn the_context_id_alone_is_a_coherent_request_and_renders_a_one_line_block() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cap = tmp.path().join("prompts");
    let m = manager_at(tmp.path());

    // EVERY injected thing off — what `inject_context: false` meant — EXCEPT the
    // context id. Formerly refused at creation, twice over.
    let mut s = ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s.injection.context_id = true;
    let id = m
        .create_thread(None, s)
        .expect("a thread may ask for the context id and nothing else");

    m.send(
        &id,
        format!("one promptfile={}", cap.display()),
        Default::default(),
        Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| captured(&cap).len() == 1).await, "delivered");
    let p = captured(&cap).remove(0);

    // THE BLOCK IS THERE, and it has exactly one line.
    assert_eq!(
        context_lines(&p),
        vec![format!("{id}.0")],
        "the line it asked for is delivered:\n{p}"
    );
    assert!(
        p.contains("kind=\"message_metadata\""),
        "the envelope exists because one of its lines does:\n{p}"
    );
    for absent in ["thread: ", "message: #", "sent: ", "idx: #", "duration: "] {
        assert!(
            !p.lines().any(|l| l.trim().starts_with(absent)),
            "{absent:?} was not asked for and must be absent:\n{p}"
        );
    }
    // And no preamble either — nothing else was asked for.
    assert!(!p.contains(&format!("[thread {id}]")), "no preamble:\n{p}");

    // ── AND OVER `threads.update`, in BOTH the directions that used to be
    // refused: raising the flag on a thread with no metadata at all, and
    // switching the metadata off under a live flag.
    let mut bare = ThreadSettings::new(tmp.path().to_string_lossy().to_string(), "fable".to_string());
    bare.injection = threads::settings::Injection::NOTHING;
    let dead = m.create_thread(None, bare).unwrap();
    let patch: SettingsPatch = serde_json::from_value(json!({"inject_context_id": true})).unwrap();
    m.update_settings(&dead, patch)
        .expect("raising the flag on a thread that injects nothing else is coherent");

    let live = m.create_thread(None, with_context_id(tmp.path())).unwrap();
    let patch: SettingsPatch = serde_json::from_value(json!({
        "inject_thread_id": false, "inject_message_id": false,
        "inject_timestamp": false, "inject_duration": false, "inject_app_idx": false,
    }))
    .unwrap();
    m.update_settings(&live, patch)
        .expect("switching every OTHER line off under a live context id is coherent");
}

/// **THE RETIRED KEYS STILL ARRIVE, AND THEY STILL MEAN WHAT THEY MEANT.**
///
/// Threads on disk carry `inject_context` / `metadata_level`, and so does the
/// AUI's create blob. The migration is: accept both, translate once inside
/// `Deserialize`, store only the new keys — so no existing thread's behaviour
/// moves and there is no second door to forget.
#[test]
fn the_retired_keys_translate_and_are_not_written_back() {
    // `metadata_level: none` → every metadata line off, nothing else touched.
    let s: ThreadSettings =
        serde_json::from_value(json!({"cwd": "/tmp", "model": "fable", "metadata_level": "none"}))
            .unwrap();
    assert!(!s.injection.any_message_metadata(), "{:?}", s.injection);
    assert!(!s.injection.queue_lead_in, "{:?}", s.injection);
    assert!(s.injection.first_turn_preamble, "{:?}", s.injection);
    assert!(s.injection.application_headers, "{:?}", s.injection);

    // `inject_context: false` → everything off; it was the broader switch and
    // it is applied after the level, which is what reproduces the old order.
    let s: ThreadSettings = serde_json::from_value(json!({
        "cwd": "/tmp", "model": "fable", "inject_context": false, "metadata_level": "standard"
    }))
    .unwrap();
    assert_eq!(s.injection, threads::settings::Injection::NOTHING);

    // `inject_context: true` is a no-op — it is how AUI says "inject normally".
    let s: ThreadSettings = serde_json::from_value(json!({
        "cwd": "/tmp", "model": "fable", "inject_context": true
    }))
    .unwrap();
    assert_eq!(s.injection, threads::settings::Injection::default());

    // `metadata_level` is GONE from what we write. `inject_context` is NOT, and
    // that is deliberate: it is one of AUI's six diffed keys and dropping it
    // would put AUI into a per-turn patch loop (`aui-data-contract.md` §5.2).
    let blob = serde_json::to_value(ThreadSettings::new("/tmp", "fable")).unwrap();
    assert!(blob.get("metadata_level").is_none(), "{blob}");
    assert_eq!(blob["inject_context"], json!(true), "the derived mirror: {blob}");

    // THE MIRROR ROUND-TRIPS, which is the property that keeps AUI out of the
    // loop: read back what we wrote and the flags are unchanged.
    for inj in [
        threads::settings::Injection::default(),
        threads::settings::Injection::NOTHING,
    ] {
        let mut s = ThreadSettings::new("/tmp", "fable");
        s.injection = inj;
        let once = serde_json::to_string(&s).unwrap();
        let back: ThreadSettings = serde_json::from_str(&once).unwrap();
        assert_eq!(back.injection, inj, "round trip: {once}");
        assert_eq!(serde_json::to_string(&back).unwrap(), once, "byte-stable");
    }

    // A patch may still speak them too.
    let mut s = ThreadSettings::new("/tmp", "fable");
    let patch: SettingsPatch = serde_json::from_value(json!({"metadata_level": "none"})).unwrap();
    let applied = s.apply(patch);
    assert!(!s.injection.any_message_metadata());
    assert_eq!(
        applied["inject_message_id"],
        json!(false),
        "the audit event names the NEW keys that moved, not a field we deleted: {applied}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// 4. THE SHAPE THE CONSUMER WAS PROMISED
// ═════════════════════════════════════════════════════════════════════════

/// The derivation, and the `{thread_id}.{n}` spelling the message-body resource
/// id already uses. Zero new state: the only input besides the id is the
/// boundary column the store already keeps.
#[test]
fn the_id_is_the_thread_id_and_the_boundary_and_nothing_else() {
    let t = "117d80e2-7c74-4054-96f1-e113ab37c245";
    assert_eq!(context_id(t, 0), format!("{t}.0"));
    assert_eq!(context_id(t, 42), format!("{t}.42"));
    assert_eq!(context_id(t, 97), format!("{t}.97"));
    // Two different boundaries can never collide, and the same boundary always
    // gives the same key — which is the entire contract a cache needs.
    assert_ne!(context_id(t, 42), context_id(t, 97));
    assert_eq!(context_id(t, 42), context_id(t, 42));
}

/// **The exact rendered line**, in both forms it can take: glossed on a
/// boundary post (which is the only place it is ever delivered) and bare if a
/// caller renders the block directly.
#[test]
fn the_rendered_line_rides_directly_under_the_thread_id() {
    let t = "117d80e2-7c74-4054-96f1-e113ab37c245";
    let cid = context_id(t, 42);
    let meta = MessageMeta {
        thread_id: Some(t),
        context_id: Some(&cid),
        message_id: Some(43),
        idx: None,
        sent: Some(SentStamp {
            ts: Local
                .with_ymd_and_hms(2026, 8, 2, 10, 0, 0)
                .unwrap()
                .with_timezone(&Utc),
            show_date: true,
        }),
        gloss: true,
        duration: None,
    };
    let block = message_metadata_notice(&meta).expect("this shape has lines");
    // The envelope indents its body, so compare the body's LINES rather than a
    // raw substring: what is being pinned is the order and the text, not the
    // indentation `system_notice` owns.
    let lines: Vec<String> = block.lines().map(|l| l.trim().to_string()).collect();
    let thread_at = lines.iter().position(|l| l == &format!("thread: {t}")).unwrap_or_else(|| panic!("no thread line:\n{block}"));
    assert_eq!(
        lines[thread_at + 1],
        format!("context: {t}.42 (this context window; a new id after each compaction)"),
        "the context line sits immediately under the thread id — same kind of \
         fact, same posts:\n{block}"
    );

    let bare = MessageMeta { gloss: false, ..meta };
    let block = message_metadata_notice(&bare).expect("this shape has lines");
    assert!(
        block.lines().any(|l| l.trim() == format!("context: {t}.42")),
        "unglossed, the line is the bare id:\n{block}"
    );

    // `None` omits the line entirely rather than rendering an empty one.
    let off = MessageMeta { context_id: None, ..bare };
    assert!(
        !message_metadata_notice(&off)
            .expect("other lines remain")
            .contains("context:"),
        "no context id means no line"
    );

    // ── THE DERIVED ENVELOPE, from the renderer's own side. Nothing on, no
    // block — which is what makes "the context id and nothing else" coherent
    // and what the deleted coherence check used to refuse.
    let nothing = MessageMeta {
        thread_id: None,
        context_id: None,
        message_id: None,
        idx: None,
        sent: None,
        gloss: false,
        duration: None,
    };
    assert!(
        message_metadata_notice(&nothing).is_none(),
        "no lines, no envelope — not an empty block and not a blank line"
    );
    // Exactly one line on, and the envelope carries exactly that line.
    let only_context = MessageMeta {
        context_id: Some(&cid),
        ..nothing
    };
    let block = message_metadata_notice(&only_context).expect("one line, so one block");
    let body: Vec<&str> = block
        .lines()
        .filter(|l| !l.trim_start().starts_with('<'))
        .map(|l| l.trim())
        .collect();
    assert_eq!(
        body,
        vec![format!("context: {t}.42")],
        "a one-line request renders a one-line block:\n{block}"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
