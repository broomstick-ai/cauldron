//! **THE RESTART ANNOUNCEMENT CHANNEL** — the daemon half.
//!
//! > "Could we add a WebSocket broadcast to the threads module, indicating that
//! >  it's going down for a restart, indicating whether it's restarting to load
//! >  a new binary or to fall back to an old binary, if it's possible to do the
//! >  latter."
//!
//! The interesting word is the last clause, and these tests are mostly about it.
//! There are TWO fallbacks in this system and they are not symmetrical:
//!
//! * A **planned** fallback — `Preflight::Restored`. The staged build failed its
//!   own preflight, the last-good copy was restored over the launch path, and
//!   the daemon is about to `execv` into the OLD image. The daemon is alive and
//!   knows. `going_down_reason` reports `fallback` and
//!   `an_armed_restart_onto_the_previous_binary_says_fallback_not_new_binary`
//!   pins it.
//! * An **unplanned** fallback — the daemon crashed and the supervisor gave up
//!   on the new binary. **No test here asserts a going-down frame for it,
//!   because there is none and there cannot be one:** the process that would
//!   have to speak is the one that died. It is reported on the way back up, from
//!   a fact the NEW process can measure about ITSELF, and
//!   `a_daemon_running_the_last_good_file_says_so_on_the_way_back_up` is that.
//!
//! Nothing here runs a real claude turn, restarts the live daemon, or execs
//! anything: the final `execv` is the one substituted step, exactly as in
//! `self_update.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::event::Push;
use threads::update::{self, LifecycleNote, Preflight, RestartReason, UpdateGate};
use threads::{build_manager, Config, ThreadManager};

#[path = "support/first_exec.rs"]
mod first_exec;

// ── fixtures (deliberately a copy of self_update.rs's, not a shared module:
//    integration tests are separate crates and a `mod common` would couple two
//    files that are otherwise free to move independently) ──────────────────

fn real_daemon_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_threads"))
}

fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    std::env::set_var("FAKE_STATE_DIR", std::env::temp_dir());
}

fn config_at(dir: &Path) -> Config {
    let mut accounts = std::collections::BTreeMap::new();
    accounts.insert(
        "aa-owned".to_string(),
        dir.join("aa-owned").to_string_lossy().to_string(),
    );
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        accounts,
        ..Default::default()
    }
}

fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(p, perm).unwrap();
}

/// The candidate is a file written by THIS call, and its first exec waits on
/// the machine's fresh-executable assessment — shared, machine-wide, and
/// longer than `SELF_CHECK_TIMEOUT` under suite load. `first_exec::warm` pays
/// that once here, so `arm()`'s preflight times the candidate, not the scanner.
/// (R9.F2: this is why the two `arm()` tests below were red in the full suite
/// with "does not pass preflight" and green alone.)
fn gate_with_candidate(data_dir: &Path, dest: &Path) -> Arc<UpdateGate> {
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::copy(real_daemon_binary(), dest).unwrap();
    make_executable(dest);
    first_exec::warm(dest);
    UpdateGate::for_binary(data_dir.to_path_buf(), dest.to_path_buf())
}

fn manager_with_gate(dir: &Path) -> (ThreadManager, Arc<UpdateGate>) {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let gate = gate_with_candidate(&data, &dir.join("threads-candidate"));
    let mgr = build_manager(config_at(&data))
        .unwrap()
        .with_update_gate(gate.clone());
    (mgr, gate)
}

type Notes = Arc<Mutex<Vec<LifecycleNote>>>;

/// Every lifecycle note that reaches a firehose subscriber.
fn watch_lifecycle(m: &ThreadManager) -> Notes {
    let mut rx = m.subscribe();
    let seen: Notes = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Lifecycle(n) = p {
                out.lock().unwrap().push(n);
            }
        }
    });
    seen
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    f()
}

// ── 1. going down: it is broadcast, and it is broadcast EARLY ────────────

/// **THE ANNOUNCEMENT RIDES `arm`, NOT `fire`.**
///
/// `fire` runs inside the quiet window holding the gate mutex and ends in
/// `execv`, which replaces every thread and every socket of this process
/// mid-flush. A frame broadcast there is still in the fan-out ring when the
/// process it belongs to stops existing. `arm` is the earliest instant at which
/// the restart is a committed fact and the last one with any slack in it, so a
/// subscriber sees the note while the daemon is fully serving and has not yet
/// closed admission.
///
/// The assertion that carries the weight is the last one: **the gate has not
/// fired.** A test that only checked "a note arrived" would pass just as
/// happily if the note were emitted from inside `fire`, which is the one place
/// it must not be.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_is_announced_while_the_socket_is_still_open() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (mgr, gate) = manager_with_gate(tmp.path());
    let notes = watch_lifecycle(&mgr);

    gate.request_restart(RestartReason::NewBinary {
        identity: "cdhash:test".into(),
    });
    gate.arm().await.expect("the real binary passes preflight");

    assert!(
        eventually(|| !notes.lock().unwrap().is_empty()).await,
        "arming a restart must reach subscribers"
    );
    let note = notes.lock().unwrap()[0].clone();
    assert_eq!(note.service, "threads");
    assert_eq!(note.phase, update::PHASE_GOING_DOWN);
    assert_eq!(note.reason, update::CAUSE_NEW_BINARY);
    assert!(
        !note.boot_id.is_empty(),
        "the note names the boot it is ending, so a client can correlate it \
         with the boot_id it holds from `welcome`"
    );

    // THE POINT OF THE TEST: the daemon has said it is going down and has not
    // gone anywhere. Nothing has been exec'd, admission is still open.
    assert!(
        gate.admitting(),
        "the announcement must be strictly BEFORE the quiet window closes — \
         a note that only leaves at `fire` never makes it out of the ring"
    );
}

/// The operator asked to hear WHY, and a hand-requested restart is not a new
/// binary. The reason is `RestartReason::kind`'s own string, not a new word.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_operator_restart_is_announced_as_an_operator_restart() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let (mgr, gate) = manager_with_gate(tmp.path());
    let notes = watch_lifecycle(&mgr);

    gate.request_restart(RestartReason::Operator {
        note: "asked for by hand".into(),
    });
    gate.arm().await.unwrap();

    assert!(eventually(|| !notes.lock().unwrap().is_empty()).await);
    assert_eq!(notes.lock().unwrap()[0].reason, update::CAUSE_OPERATOR);
}

/// **THE PLANNED FALLBACK — the half of "if it's possible to do the latter"
/// that IS possible.**
///
/// `Preflight::Restored` means the staged build did not run, last-good was
/// restored over the launch path, and the daemon is about to become the OLD
/// image. It is alive, it is serving, and it knows: so it says `fallback` and
/// not `new_binary`, which is the distinction the operator asked for.
#[test]
fn an_armed_restart_onto_the_previous_binary_says_fallback_not_new_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let gate = gate_with_candidate(&data, &tmp.path().join("threads-candidate"));
    gate.request_restart(RestartReason::NewBinary {
        identity: "cdhash:the-build-that-did-not-run".into(),
    });

    let (ok, _) = gate.going_down_reason(Preflight::Ok);
    let (restored, detail) = gate.going_down_reason(Preflight::Restored);

    assert_eq!(
        ok,
        update::CAUSE_NEW_BINARY,
        "a healthy staged build is a new binary"
    );
    assert_eq!(
        restored,
        update::CAUSE_FALLBACK,
        "the SAME queued NewBinary reason must announce as `fallback` once the \
         preflight has decided we are restarting onto the previous image — the \
         pending record says what we WANTED, the preflight says what we are \
         actually about to become, and the operator asked to hear the latter"
    );
    assert!(
        detail.contains("previous binary"),
        "detail: {detail}"
    );
}

// ── 2. coming back up: the fallback the daemon could not announce ────────

/// **THE UNPLANNED FALLBACK, REPORTED HONESTLY — ON THE WAY UP.**
///
/// The supervisor's fallback path execs `<data_dir>/threads.last-good` directly,
/// so the new process's own `current_exe()` IS that file. That is a fact this
/// process can measure about itself with two `stat`s, and it is the only honest
/// route to "I came up on the fallback image": the process that could have
/// announced the going-down is the one that died.
///
/// Compared by `(dev, ino)` rather than by path string, because the supervisor
/// builds its path from `$THREADS_DATA_HOME` and the kernel answers
/// `current_exe()` — the two agree on the file, not on the characters.
#[tokio::test]
async fn a_daemon_running_the_last_good_file_says_so_on_the_way_back_up() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();

    // Exactly what the supervisor does: run the last-good copy itself.
    let last_good = data.join("threads.last-good");
    std::fs::copy(real_daemon_binary(), &last_good).unwrap();
    make_executable(&last_good);
    let gate = UpdateGate::for_binary(data.clone(), last_good.clone());

    gate.on_boot().await;

    let note = gate.boot_note().expect("every boot accounts for itself");
    assert_eq!(note.phase, update::PHASE_BACK_UP);
    assert_eq!(
        note.reason,
        update::CAUSE_FALLBACK,
        "a daemon whose own executable IS the last-good file came up on the \
         fallback image, and that is the one fallback statement it can make \
         without inventing anything"
    );
}

/// The ordinary case, and the guard against the test above passing for the
/// wrong reason: a daemon running its NORMAL binary must not claim a fallback
/// just because a last-good copy happens to exist beside it.
#[tokio::test]
async fn an_ordinary_boot_does_not_claim_a_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let last_good = data.join("threads.last-good");
    std::fs::copy(real_daemon_binary(), &last_good).unwrap();
    make_executable(&last_good);

    // A DIFFERENT file with the same contents — same bytes, different inode,
    // which is precisely the case a path-string or a hash comparison would get
    // wrong in one direction or the other.
    let normal = tmp.path().join("threads");
    std::fs::copy(real_daemon_binary(), &normal).unwrap();
    make_executable(&normal);
    let gate = UpdateGate::for_binary(data.clone(), normal);

    gate.on_boot().await;

    let note = gate.boot_note().unwrap();
    assert_eq!(note.phase, update::PHASE_BACK_UP);
    assert_ne!(
        note.reason,
        update::CAUSE_FALLBACK,
        "identical CONTENT is not the fallback signal — running the last-good \
         FILE is. Two copies of one build are not a rollback."
    );
}

/// A boot that cannot account for itself says `unknown`, and `unknown` is a real
/// answer. The client renders it as "it restarted" with no because — which is
/// the whole point of having the word, rather than defaulting to `new_binary`
/// and telling him something that may not be true.
#[tokio::test]
async fn a_boot_with_nothing_to_go_on_says_unknown_rather_than_guessing() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let normal = tmp.path().join("threads");
    std::fs::copy(real_daemon_binary(), &normal).unwrap();
    make_executable(&normal);
    let gate = UpdateGate::for_binary(data, normal);

    gate.on_boot().await;

    assert_eq!(gate.boot_note().unwrap().reason, update::CAUSE_UNKNOWN);
}

/// A boot that SATISFIED a queued new-binary restart reports it as one, so the
/// interface can say "it came back on a new binary" even when the going-down
/// frame never made it out.
#[tokio::test]
async fn a_boot_that_satisfied_a_queued_restart_reports_that_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let normal = tmp.path().join("threads");
    std::fs::copy(real_daemon_binary(), &normal).unwrap();
    make_executable(&normal);
    let gate = UpdateGate::for_binary(data.clone(), normal.clone());

    // Queue a restart FOR THE IDENTITY WE ARE ABOUT TO BOOT WITH — which is
    // what an absorbed deploy looks like from the next process's side.
    let identity = update::binary_identity(&normal).unwrap();
    gate.request_restart(RestartReason::NewBinary { identity });

    // A fresh gate over the same data dir: a new process, reading the record
    // the old one left.
    let next = UpdateGate::for_binary(data, normal);
    next.on_boot().await;

    assert_eq!(next.boot_note().unwrap().reason, update::CAUSE_NEW_BINARY);
}

// ── 3. the line a consumer reads ─────────────────────────────────────────

/// The note reaches a consumer as a flat line tagged `lifecycle`, and it
/// carries the service id, so a second service emitting it needs no new client
/// code.
///
/// **RE-CUT AT R2.S4.** Before: this serialised the legacy
/// `protocol::ServerMsg::Lifecycle` frame and asserted `type: "lifecycle"`.
/// That frame went with the firehose; the fact is the `narration` shape's
/// `lifecycle` line (and, for this boot's own note, the `boot` shape's row).
/// Now it serialises `narration::Line::Lifecycle` — the line a narration
/// subscriber is handed — and asserts the same four things on it, with `kind`
/// as the tag where `type` was.
///
/// **Forced red** (R2.S4) by tagging `narration::Line` with `type` instead of
/// `kind`: `left: Null`, `right: "lifecycle"`.
#[test]
fn the_wire_frame_is_flat_tagged_and_names_the_service() {
    let line = threads::shapes::narration::Line::Lifecycle {
        service: "tts".into(),
        phase: update::PHASE_GOING_DOWN.into(),
        reason: update::CAUSE_NEW_BINARY.into(),
        detail: "restarting to load a new binary".into(),
        boot_id: "b-1".into(),
        at: "2026-08-31T00:00:00+00:00".into(),
    };
    let v: serde_json::Value = serde_json::to_value(&line).unwrap();
    assert_eq!(v["kind"], "lifecycle");
    assert_eq!(
        v["service"], "tts",
        "the channel is not a threads feature — the operator asked for text to \
         speech and 'any other systems that we end up subscribing to' on the \
         SAME channel, so the service is a field and never the line's identity"
    );
    assert_eq!(v["phase"], "going_down");
    assert_eq!(v["reason"], "new_binary");
}

/// The reason vocabulary is DERIVED, not invented: the two restart reasons the
/// daemon already records are the two reason strings on the wire, character for
/// character. If someone renames `RestartReason::kind`'s output, this fails.
#[test]
fn the_reason_words_are_the_ones_the_restart_ledger_already_uses() {
    assert_eq!(
        RestartReason::NewBinary {
            identity: "x".into()
        }
        .kind(),
        update::CAUSE_NEW_BINARY
    );
    assert_eq!(
        RestartReason::Operator { note: "x".into() }.kind(),
        update::CAUSE_OPERATOR
    );
    assert_eq!(
        Preflight::Restored.as_str(),
        "restored-last-good",
        "the fallback reason's evidence is this decision; it is named here so a \
         rename of one without the other is a failing test rather than a \
         silently wrong announcement"
    );
}
