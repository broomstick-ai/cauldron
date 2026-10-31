//! THE DAEMON'S UPDATE PROTOCOL — operator msg #1630, intent item 64.
//!
//! > "How are you gonna restart it if you're running on it and you're required
//! >  to not be running? You need to put in an update protocol. You need it to
//! >  look for its own binary and to restart in that first moment where nothing
//! >  is running and nothing is being started."
//!
//! Every test here drives the REAL mechanism — the real admission gate, the real
//! quiet window, the real preflight running a real `threads --self-check` — and
//! substitutes only the final `execv`, which cannot be exercised in-process
//! because on success it does not return.
//!
//! Nothing here runs a real claude turn (the fake-claude fixture is the only
//! `claude`), nothing creates `~/.threads` (every manager gets an explicit
//! `data_dir` under a tempdir), nothing touches the operator's login, nothing
//! kills a process, and nothing restarts the live daemon.
//!
//! ## What the watch is keyed on, since 2026-08-17: a SIGNATURE
//!
//! Not the bytes. `target/debug/threads` is written by every `cargo build` and
//! every `cargo check` any agent in this shared tree runs, so a content watch
//! made a compile check a deploy. The daemon now absorbs only an image whose
//! designated requirement names `com.harness.threads` — i.e. one a human put
//! there with `lib/threads/scripts/build-threads.sh`.
//!
//! Two tests own that contract and they are a matched pair; do not delete one:
//! `an_unsigned_rebuild_is_not_a_deploy_so_a_compile_check_is_a_no_op` and
//! `a_signed_build_is_the_absorb_signal_and_queues_a_restart`. The negative is
//! keychain-free and always runs; the positive needs the `harness-codesign`
//! identity and says so out loud when it is missing.
//!
//! ## The two things a later reader must not "fix"
//!
//! `parked_turns_and_a_full_queue_do_not_block_a_restart` is deliberate, not an
//! oversight. Parked turns, waiting queue rows and armed retries are DURABLE and
//! are the daemon's normal steady state; making them part of the quiet condition
//! turns this into a mechanism that never restarts. "Nothing is being started" is
//! enforced by closing admission, not by waiting for the backlog to drain.
//!
//! `running_turns_are_not_a_reason_to_wait_and_the_spawn_window_is` is the same
//! shape and is the newer of the two. A RUNNING TURN IS NOT A BLOCKER: it
//! survives the `execv` and is re-adopted at boot, and waiting for every turn in
//! the fleet to end at the same instant starved a restart for 266 minutes on
//! this machine. The one window a restart does wait for is the microseconds
//! between `cmd.spawn()` and `upsert_running_turn`, and that test owns both
//! halves of the contract. Do not restore `turns` to `Blockers::quiet`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use threads::event::{Push, SendIntent, ThreadEvent};
use threads::runtime::LockRefusal;
use threads::settings::ThreadSettings;
use threads::store::ParkedTurnRow;
use threads::update::{
    self, Blockers, ExecHook, Preflight, RestartReason, UpdateGate, WindowOutcome, SELF_CHECK_OK,
};
use threads::{build_manager, Config, ThreadManager};

#[path = "support/first_exec.rs"]
mod first_exec;


// ── THE SERIAL GATE ──────────────────────────────────────────────────────────
//
// **Every test in this file makes a TIMING assertion** — preflight windows,
// spawn windows, defer-and-fire ordering — and every one of them spawns real
// daemon processes off the one `CARGO_BIN_EXE_threads`. `cargo test` runs a
// binary's tests on parallel threads, so run together they starve each other and
// 3 to 7 of them fail AT RANDOM: the same commit came back green and then with
// six failures an hour later with nothing changed but scheduling.
//
// That made this binary's result a coin flip reported as a gate, which is worse
// than a known-failing test: a green here was being quoted as evidence.
//
// This serialises the file against itself. Every OTHER test binary stays
// parallel, so the suite loses only this file's internal concurrency — which was
// never real, since these tests contend for the machine by design.
//
// Poisoning is absorbed deliberately: one failing test must not convert every
// later one into a panic that hides its own reason.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ── fixtures ─────────────────────────────────────────────────────────────

/// The REAL `threads` binary cargo just built. It is the honest candidate: it is
/// a genuine Mach-O of the right size and it genuinely answers `--self-check`.
fn real_daemon_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_threads"))
}

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
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

fn config_at(dir: &Path) -> Config {
    // CON-27: a restart-window test has to be able to START A TURN — that is the
    // whole thing it holds the window against. The machine `default` that
    // `ensure_default` adds on boot is no longer a place a thread may run, so a
    // config with no accounts of its own can no longer create a thread at all,
    // and the test would fail before it ever reached the gate. So register one
    // account threads OWNS: a name with its own config dir under this fixture's
    // data dir. It is only ever spawned against the fake claude, which ignores
    // `CLAUDE_CONFIG_DIR`, so nothing needs to exist at that path.
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

fn plain(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

/// Copy the real daemon binary to `dest` and hand back a gate pinned to it.
/// The candidate is a COPY so a test can corrupt it without touching the build.
fn gate_with_candidate(data_dir: &Path, dest: &Path) -> Arc<UpdateGate> {
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::copy(real_daemon_binary(), dest).unwrap();
    make_executable(dest);
    // R9.F2: pay the fresh file's first exec here, outside every timed
    // preflight. See `support/first_exec.rs`.
    first_exec::warm(dest);
    UpdateGate::for_binary(data_dir.to_path_buf(), dest.to_path_buf())
}

fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(p, perm).unwrap();
}

// ── the two kinds of build, as fixtures ──────────────────────────────────
//
// Everything about the absorb signal turns on the difference between these two,
// so they are built with the real `codesign`, not simulated. Byte-poking a
// Mach-O is NOT a usable stand-in and a later reader should not "simplify" them
// into one: `codesign -dvvv` REPORTS the stored code directory rather than
// recomputing page hashes, so appending or flipping bytes in a signed Mach-O
// leaves the CDHash — and therefore the code identity — completely unchanged.
// (Measured 2026-08-17. The old fixture in this file did exactly that and only
// worked because identity used to be a raw SHA-256.)

/// Re-link simulation: what a bare `cargo build` / `cargo check` leaves behind.
///
/// An AD-HOC signature — `codesign --sign -`, which is what the linker itself
/// applies on Apple Silicon. It gives the file a NEW CDHash (so this really is
/// "different code arrived", not a touch) while leaving the designated
/// requirement a bare `cdhash H"…"` that names no identifier. Needs no keychain,
/// which is the point: this half of the contract is machine-independent.
fn fake_unsigned_rebuild(path: &Path, tag: &str) {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--identifier"])
        .arg(tag)
        .arg(path)
        .output()
        .expect("codesign runs");
    assert!(
        out.status.success(),
        "ad-hoc sign failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !update::is_stably_signed(path),
        "an ad-hoc signature must never look like a deploy"
    );
}

/// The deploy: exactly what `lib/threads/scripts/build-threads.sh` does, by
/// calling the same signer with the same identity and identifier.
///
/// Returns false when the `harness-codesign` identity is not in this machine's
/// keychain. There is no way around that dependency and no honest way to fake
/// it: [`update::is_stably_signed`] reads the DESIGNATED REQUIREMENT, which our
/// signer makes name both the identifier and the certificate leaf, so producing
/// one requires the private key. That is the property that stops a stray
/// `cargo build` from forging a deploy, so weakening the check to make the test
/// self-contained would delete the feature.
fn sign_for_deploy(path: &Path) -> bool {
    let have_identity = std::process::Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("harness-codesign"))
        .unwrap_or(false);
    if !have_identity {
        eprintln!(
            "SKIPPING the signed half: no 'harness-codesign' identity in the keychain. \
             Run harness/scripts/setup-codesign.sh once. The UNSIGNED half of this \
             contract — the one that makes a compile check a no-op — still ran."
        );
        return false;
    }
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "harness-codesign", "--identifier"])
        .arg(update::STABLE_BUNDLE_ID)
        .arg(path)
        .output()
        .expect("codesign runs");
    assert!(
        out.status.success(),
        "signing with harness-codesign failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        update::is_stably_signed(path),
        "a binary signed with our identity and identifier must read as deployable"
    );
    true
}

/// A manager whose update gate is aimed at a fixture copy of the daemon binary.
fn manager_with_gate(dir: &Path) -> (ThreadManager, Arc<UpdateGate>) {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let gate = gate_with_candidate(&data, &dir.join("threads-candidate"));
    let mgr = build_manager(config_at(&data))
        .unwrap()
        .with_update_gate(gate.clone());
    (mgr, gate)
}

/// The same, but with the candidate literally NAMED `threads`, so
/// `watch_wanted()` is true and the real `spawn_watcher` loop can be exercised.
/// The exec hook is always installed before the watcher starts, so the fixture
/// is never actually exec'd.
fn manager_with_watchable_gate(dir: &Path) -> (ThreadManager, Arc<UpdateGate>) {
    let data = dir.join("data");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let gate = gate_with_candidate(&data, &bin.join("threads"));
    let mgr = build_manager(config_at(&data))
        .unwrap()
        .with_update_gate(gate.clone());
    (mgr, gate)
}

/// An exec hook that records that it was reached and reports success — i.e.
/// "from here the process would be gone".
fn recording_hook(flag: Arc<AtomicBool>) -> ExecHook {
    Box::new(move |_| {
        flag.store(true, Ordering::SeqCst);
        Ok(())
    })
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

type Log = Arc<Mutex<Vec<ThreadEvent>>>;

fn watch(m: &ThreadManager) -> Log {
    let mut rx = m.subscribe();
    let seen: Log = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            if let Push::Event(ev) = p {
                out.lock().unwrap().push(ev.event);
            }
        }
    });
    seen
}

fn count_starts(log: &Log) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, ThreadEvent::TurnStarted { .. }))
        .count()
}

fn log_lines(gate: &UpdateGate) -> Vec<serde_json::Value> {
    std::fs::read_to_string(gate.log_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn events_in_log(gate: &UpdateGate) -> Vec<String> {
    log_lines(gate)
        .iter()
        .filter_map(|v| v["event"].as_str().map(str::to_string))
        .collect()
}

// ── 1. the watch: a SIGNATURE, not mtime and not bytes ───────────────────

/// **THE NO-OP REQUIREMENT.** A compile check must not deploy anything.
///
/// Operator, verbatim: *"Whoever did the change does the build. That's separate
/// from making a compile, though. Checking that it compiles needs to be reserved
/// as a no-op in terms of restarting the daemon."*
///
/// Many agents share `target/debug`. Before the signature gate, every
/// `cargo build` and every `cargo check` any of them ran restarted the live
/// daemon into code nobody had decided to ship.
///
/// Three things must all be no-ops here, and the third is the new one:
/// an unchanged binary, a touch, and a genuinely DIFFERENT unsigned build.
///
/// Forced to fail before it was trusted: pointing `scan_binary` back at
/// `binary_identity` instead of `absorbable_identity` fails the third
/// assertion — `a bare cargo build must never queue a restart` — because the
/// ad-hoc re-link really does produce a new CDHash. The identity moving is
/// exactly why gating on identity alone was not enough.
#[tokio::test]
async fn an_unsigned_rebuild_is_not_a_deploy_so_a_compile_check_is_a_no_op() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let candidate = dir.path().join("threads-candidate");
    let gate = gate_with_candidate(&data, &candidate);

    gate.scan_binary();
    assert!(gate.pending().is_none(), "an unchanged binary queues nothing");

    // Rewrite the SAME bytes: mtime moves, identity does not.
    let bytes = std::fs::read(&candidate).unwrap();
    std::fs::write(&candidate, &bytes).unwrap();
    make_executable(&candidate);
    gate.scan_binary();
    assert!(
        gate.pending().is_none(),
        "a touch that produced identical bytes must not queue a restart"
    );

    // A bare `cargo build`: genuinely new code, genuinely a new CDHash, and
    // genuinely NOT a deploy. Assert the identity really did move first, so
    // this can never pass for the boring reason that nothing changed.
    let before = update::binary_identity(&candidate).expect("the fixture fingerprints");
    fake_unsigned_rebuild(&candidate, "some-other-build");
    make_executable(&candidate);
    let after = update::binary_identity(&candidate).expect("the fixture still fingerprints");
    assert_ne!(before, after, "the fixture must really be a different build");
    assert!(
        update::absorbable_identity(&candidate).is_none(),
        "an unsigned build has no absorbable identity at all — it is not a deploy"
    );

    gate.scan_binary();
    assert!(
        gate.pending().is_none(),
        "a bare cargo build must never queue a restart — checking that it compiles \
         is a no-op"
    );
    assert!(
        !events_in_log(&gate).contains(&"queued".to_string()),
        "and it does not even reach the decision ledger"
    );
    assert!(!gate.pending_path().exists(), "nothing durable was written either");
}

/// **THE DEPLOY SIGNAL.** Signing IS the absorb instruction.
///
/// Operator, verbatim: *"instead of moving it to a new binary, I actually think
/// you need to do the same thing that AUI is doing by signing the binary,
/// ideally with the same thing that's doing the signing for AUI."*
///
/// Same starting point as the test above — an unsigned build that queued
/// nothing. The ONLY thing that happens between the ignored scan and the queued
/// scan is `codesign` with the harness identity.
///
/// Forced to fail before it was trusted: signing with `--sign -` (ad-hoc)
/// instead of `harness-codesign` leaves the DR without our identifier and the
/// `expect("…queues a restart")` blows up.
#[tokio::test]
async fn a_signed_build_is_the_absorb_signal_and_queues_a_restart() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let candidate = dir.path().join("threads-candidate");
    let gate = gate_with_candidate(&data, &candidate);

    // Establish the negative in the SAME gate, so the positive below is
    // attributable to the signature and to nothing else.
    fake_unsigned_rebuild(&candidate, "the-compile-check");
    make_executable(&candidate);
    gate.scan_binary();
    assert!(gate.pending().is_none(), "still just a compile check");

    if !sign_for_deploy(&candidate) {
        return; // no keychain identity — see sign_for_deploy's doc comment.
    }
    make_executable(&candidate);
    let deployed = update::absorbable_identity(&candidate)
        .expect("a signed build has an absorbable identity");

    gate.scan_binary();
    let pending = gate.pending().expect("a SIGNED build queues a restart");
    assert_eq!(pending.kind, "new_binary");
    assert_eq!(
        pending.identity.as_deref(),
        Some(deployed.as_str()),
        "the queued restart names the signed image it is waiting to become"
    );

    // Durable enough to survive a long defer AND a daemon exit.
    let on_disk: threads::update::PendingRestart =
        serde_json::from_str(&std::fs::read_to_string(gate.pending_path()).unwrap()).unwrap();
    assert_eq!(on_disk, pending, "the queued restart is written to disk verbatim");
    assert!(events_in_log(&gate).contains(&"queued".to_string()));

    // AND the trap that cost the AUI a restart loop: re-signing the same code
    // must not read as yet another new build.
    //
    // The two hashes are compared as HEX, never as byte vectors — a failed
    // `assert_eq!` on two 27 MB `Vec<u8>`s prints both of them, which is how
    // this test once produced a 220 MB test log.
    //
    // The sleep is load-bearing, not padding: the difference between the two
    // signed files is the CMS signing TIME, whose resolution is one second. Sign
    // twice inside the same second and the bytes come out identical, and the
    // trap this asserts becomes invisible.
    let sha_before = sha256_hex(&candidate);
    std::thread::sleep(Duration::from_millis(1100));
    assert!(sign_for_deploy(&candidate));
    let sha_after = sha256_hex(&candidate);
    assert_ne!(
        sha_before, sha_after,
        "a re-sign rewrites the file — which is exactly why a whole-file hash is \
         the WRONG fingerprint for this decision"
    );
    assert_eq!(
        update::absorbable_identity(&candidate).as_deref(),
        Some(deployed.as_str()),
        "…while the CDHash is unchanged: a re-sign of the same code is the SAME \
         deploy. Fingerprint the file instead and the daemon restarts, re-signs, \
         and restarts again — the loop apps/aui/src/restart.rs had to be \
         rewritten to stop"
    );
}

/// Hex SHA-256 of a file, for the one assertion that has to talk about raw
/// bytes. Shelling out to `shasum` keeps `sha2` out of this crate's dev-deps.
fn sha256_hex(path: &Path) -> String {
    let out = std::process::Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("shasum runs");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The durable record is reconciled against the image we actually booted from:
/// a STALE image cannot clear it, the NEW image does, and an identity nothing on
/// this machine carries never can.
///
/// ## Why the two operands have to be two files, and what this replaces
///
/// The version this supersedes took `identity` off `gate.boot_identity()` and
/// then booted a second gate **over the same unchanged file**. Both sides of
/// `on_boot`'s comparison were therefore one `binary_identity` of one set of
/// bytes, read twice. The `satisfied` half of it could not fail: a daemon that
/// never exec'd anything and a daemon that came up on a new image are the same
/// measurement to an assertion whose two operands are the same read, so it
/// passed either way and proved only that `==` is reflexive.
///
/// That gap is not theoretical here, and the reason is the thing a later reader
/// is most likely to get wrong: **this daemon self-updates by `execv`. It
/// replaces its own executing image and KEEPS its pid, its ppid and its
/// reported start time.** "A process cannot change its executing image without
/// restarting" is false, and every cheap liveness signal — pid alive, parent
/// unchanged, uptime still climbing — survives the update unchanged. So pid
/// liveness is not evidence of anything, and "did the update take effect?" has
/// exactly one honest answer: compare the identity the durable record was
/// WAITING FOR against the identity of the image the process is ACTUALLY
/// RUNNING. Two files, two reads. That is what `on_boot` does, and it is what
/// this test now exercises instead of asserting past.
///
/// Forced red before it was trusted, with a genuinely stale image: skipping the
/// `fs::copy` that puts the new image on the launch path in half two — i.e.
/// leaving the old image exactly where it was, which is what a self-update that
/// silently did not happen looks like — fails `"the boot that satisfied the
/// record clears it"`, because the record correctly stays pending. The
/// assertion this replaces stayed green under that same change, which is how it
/// was found.
#[tokio::test]
async fn a_queued_restart_is_cleared_by_the_new_image_and_never_by_a_stale_one() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    // THE LAUNCH PATH. Whatever file is sitting here is the image a boot comes
    // up on, which is the whole point: the `execv` changes this file's contents
    // and nothing else observable about the process.
    let launch = dir.path().join("threads-candidate");

    // ── two genuinely different images ───────────────────────────────────────
    // A real ad-hoc `codesign` with its own identifier, which mints a fresh code
    // directory and therefore a genuinely different `binary_identity`. Poking
    // bytes would NOT — see `fake_unsigned_rebuild`'s comment for why.
    let newer = dir.path().join("threads-newer");
    std::fs::copy(real_daemon_binary(), &newer).unwrap();
    make_executable(&newer);
    fake_unsigned_rebuild(&newer, "the-build-we-are-waiting-to-become");
    make_executable(&newer);

    let gate = gate_with_candidate(&data, &launch);
    let old_identity = gate.boot_identity().expect("the old image fingerprints").to_string();
    let new_identity = update::binary_identity(&newer).expect("the new image fingerprints");
    assert_ne!(
        old_identity, new_identity,
        "THE SENTINEL: the two fixtures must really be two different images, or \
         every assertion below passes for the boring reason that there was only \
         ever one"
    );

    // The daemon has been up on the OLD image for a while, so last-good tracks
    // it. This is the state a real deploy lands on top of.
    gate.on_boot().await;

    // ── HALF ONE: THE STALE IMAGE ───────────────────────────────────────────
    // The new build is queued and the `execv` did not happen — the file on the
    // launch path is still the old one. Nothing about the PROCESS would tell us.
    gate.request_restart(RestartReason::NewBinary {
        identity: new_identity.clone(),
    });
    let stale = UpdateGate::for_binary(data.clone(), launch.clone());
    assert_eq!(
        stale.boot_identity(),
        Some(old_identity.as_str()),
        "the launch path still holds the OLD image — this boot is the stale one"
    );
    stale.on_boot().await;
    assert!(
        stale.pending().is_some(),
        "a restart we have not performed must survive the exit that deferred it, \
         and a STALE image must not be able to clear it"
    );
    let note = stale.boot_note().expect("every boot accounts for itself");
    assert_ne!(
        note.reason,
        update::CAUSE_NEW_BINARY,
        "and this boot must not claim it came back on a new binary: {note:?}"
    );
    assert_eq!(
        note.reason,
        update::CAUSE_FALLBACK,
        "it is corroborated as the last-good image instead — identity, not a \
         guess from the disappointment: {note:?}"
    );

    // ── HALF TWO: THE UPDATE ACTUALLY TOOK ──────────────────────────────────
    // The new image is on the launch path, which is exactly what the `execv`
    // leaves behind. The record and the running image now name the same build:
    // two reads of two different files that agree, not one read compared with
    // itself.
    std::fs::copy(&newer, &launch).unwrap();
    make_executable(&launch);
    first_exec::warm(&launch); // R9.F2: `reborn.on_boot` self-checks this new file
    let reborn = UpdateGate::for_binary(data.clone(), launch.clone());
    assert_eq!(
        reborn.boot_identity(),
        Some(new_identity.as_str()),
        "the launch path holds the NEW image now, and the gate reads it off the file"
    );
    reborn.on_boot().await;
    assert!(
        reborn.pending().is_none(),
        "the boot that satisfied the record clears it — otherwise the daemon restart-loops"
    );
    assert!(events_in_log(&reborn).contains(&"satisfied".to_string()));
    let note = reborn.boot_note().expect("every boot accounts for itself");
    assert_eq!(
        note.reason,
        update::CAUSE_NEW_BINARY,
        "and THIS boot is the one entitled to say it came back on a new binary: {note:?}"
    );

    // ── AND AN IDENTITY NO FILE HERE CARRIES ────────────────────────────────
    // The arm that keeps a record from being satisfied by an image that does not
    // exist — and that a `boot_identity` of `None` must not read as a match.
    reborn.request_restart(RestartReason::NewBinary {
        identity: "sha256:something-else".into(),
    });
    let never = UpdateGate::for_binary(data.clone(), launch.clone());
    never.on_boot().await;
    assert!(
        never.pending().is_some(),
        "an identity nothing on this machine carries is never satisfied by booting"
    );
}

// ── 2. preflight and last-good ───────────────────────────────────────────

/// **Requirement 5.** A candidate that is not a working copy of this daemon
/// never goes live. With a last-good copy present it is REPLACED by it; with no
/// last-good copy the restart is REFUSED and we stay on the running build.
///
/// Fails without the preflight: `prepare` returns `Ok` for a text file and the
/// window execs it, which on a real machine means the operator has no daemon.
#[tokio::test]
async fn a_preflight_failure_refuses_to_exec_and_says_so() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let candidate = dir.path().join("threads-candidate");
    // A plausible SIZE, an executable BIT, and complete garbage inside — the
    // shape of a build interrupted or a wrong file copied into place.
    std::fs::write(&candidate, vec![b'x'; (update::MIN_BINARY_BYTES + 4096) as usize]).unwrap();
    make_executable(&candidate);
    let gate = UpdateGate::for_binary(data.clone(), candidate.clone());

    gate.request_restart(RestartReason::Operator {
        note: "deploy the new build".into(),
    });
    let err = gate.prepare().await.expect_err("garbage must not preflight");
    assert!(err.contains("REFUSING"), "the refusal says so out loud: {err}");
    // The refusal must still SAY WHY, but not by citing the absence of a
    // supervisor: there has been a launchd job + supervisor since 2026-08-17
    // (see `update::decide`). The reason that survived is the one about execv.
    assert!(
        err.contains("refusing an execv") && err.contains("no daemon at all"),
        "and says why refusing is right here: {err}"
    );
    assert!(
        !err.contains("no supervisor"),
        "and does NOT argue from a fact that stopped being true: {err}"
    );
    assert!(gate.admitting(), "a refused restart leaves the daemon serving normally");
    assert!(events_in_log(&gate).contains(&"refused".to_string()));

    // And the whole attempt reports Refused rather than firing.
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));
    match gate.attempt().await {
        Some(WindowOutcome::Refused(_)) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(!hit.load(Ordering::SeqCst), "nothing was exec'd");
}

/// A bad build cannot leave him with no daemon: the last-good copy is restored
/// over it and the restart proceeds into something that works.
#[tokio::test]
async fn a_bad_build_is_replaced_by_the_last_good_copy() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let candidate = dir.path().join("threads-candidate");

    // First: a healthy daemon boots and promotes itself to last-good.
    std::fs::copy(real_daemon_binary(), &candidate).unwrap();
    make_executable(&candidate);
    first_exec::warm(&candidate); // R9.F2: `on_boot`/`prepare` self-check it on a 20 s clock
    let gate = UpdateGate::for_binary(data.clone(), candidate.clone());
    gate.on_boot().await;
    assert!(
        gate.last_good_path().is_file(),
        "a daemon that came up records the image it came up from"
    );
    // R9.F2: `prepare` below self-checks the last-good file `on_boot` just
    // WROTE, on a 20 s clock; pay its first exec here. (The copy `prepare`
    // restores over the candidate is written and exec'd inside `prepare` and
    // cannot be warmed from a test — see the R9.F2 report.)
    first_exec::warm(&gate.last_good_path());

    // Then: a broken build lands on top of it.
    std::fs::write(&candidate, vec![b'x'; (update::MIN_BINARY_BYTES + 4096) as usize]).unwrap();
    make_executable(&candidate);
    let gate = UpdateGate::for_binary(data.clone(), candidate.clone());
    let ready = gate.prepare().await.expect("the last-good copy rescues it");
    assert_eq!(ready.preflight, Preflight::Restored);
    assert!(
        ready.banner.contains(SELF_CHECK_OK),
        "the restored binary was made to prove it runs: {}",
        ready.banner
    );
    // The candidate on disk IS the good binary now.
    assert_eq!(
        update::binary_identity(&candidate),
        update::binary_identity(&gate.last_good_path())
    );

    // The policy itself, with no filesystem at all.
    assert_eq!(update::decide(true, true), Preflight::Ok);
    assert_eq!(update::decide(false, true), Preflight::Restored);
    assert_eq!(update::decide(false, false), Preflight::NoFallback);
}

// ── 2a. last-good is named by a COMMIT, and the copy stops orphaning ─────
//
// Measured on the operator's live store on 2026-10-08, which is why these exist:
//
//   -rwxr-xr-x  uchg  16420144  Oct  7 16:15  threads.copying
//   -rwxr-xr-x  -     45543120  Oct  7 12:51  threads.last-good
//
// `promote` holds the deploy path `chflags uchg` on purpose — it is the only
// guard that survives a fully cached cargo uplift (see `lib/threads/build.rs`).
// `std::fs::copy` goes through macOS `copyfile`, which copies `st_flags` along
// with the bytes, so the temp was born immutable and could then be neither
// renamed nor unlinked. Every boot after the first failed EARLIER still, at the
// copy, because the temp in the way could not be opened.

/// Set the user immutable flag the way `promote` does, and clear it again on the
/// way out **even if the test panics**.
///
/// The guard is not tidiness. `tempfile::TempDir` removes its directory on drop
/// with `remove_dir_all`, which cannot unlink an immutable file, so a FAILING
/// test silently leaks its whole tempdir — and the fixture here is a 16 MB copy
/// of the real daemon. Measured while forcing these tests red: two orphaned
/// tempdirs holding three immutable files, which had to be cleared by hand. A
/// test about un-removable files must not be the thing that leaves them around.
#[must_use = "the flag is cleared when this guard drops; dropping it immediately               clears the flag immediately"]
struct Immutable(PathBuf);

impl Immutable {
    fn set(p: &Path) -> Self {
        let st = std::process::Command::new("/usr/bin/chflags")
            .arg("uchg")
            .arg(p)
            .status()
            .expect("chflags runs");
        assert!(st.success(), "chflags uchg {} failed", p.display());
        Immutable(p.to_path_buf())
    }
}

/// Clear file flags under `dir` on the way out, so the tempdir can be removed.
///
/// Separate from [`Immutable`] because it covers what the CODE UNDER TEST leaves
/// behind rather than what the fixture set: a broken `keep_last_good` creates an
/// immutable `threads.copying` itself, and that file wedges `TempDir`'s cleanup
/// exactly as a fixture would. Without this, every red run of these two tests
/// leaks its tempdir — measured.
struct ClearFlags(PathBuf);

impl Drop for ClearFlags {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/chflags")
            .arg("-R")
            .arg("nouchg")
            .arg(&self.0)
            .status();
    }
}

impl Drop for Immutable {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/chflags")
            .arg("nouchg")
            .arg(&self.0)
            .status();
    }
}

fn is_immutable(p: &Path) -> bool {
    // `st_flags` is the macOS extension; the portable `MetadataExt` has no
    // notion of file flags, which is the same gap that let `fs::copy` carry one
    // across without Rust having a name for it.
    use std::os::macos::fs::MetadataExt;
    // `UF_IMMUTABLE` is 0x2.
    std::fs::metadata(p).map(|m| m.st_flags() & 0x2 != 0).unwrap_or(false)
}

/// **The defect.** A daemon whose own image is immutable — which every DEPLOYED
/// daemon's is — still records that image as last-good, and leaves no temp
/// behind.
///
/// Fails against the code this replaces on both counts at once: the `rename`
/// returns `EPERM`, so `threads.last-good` keeps whatever it had, and the
/// `remove_file` on the error path returns `EPERM` too, so a 16 MB immutable
/// file is left in the data dir for ever.
#[tokio::test]
async fn an_immutable_running_image_is_still_recorded_as_last_good() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let _flags = ClearFlags(dir.path().to_path_buf());
    let candidate = dir.path().join("threads-deployed");
    std::fs::copy(real_daemon_binary(), &candidate).unwrap();
    make_executable(&candidate);
    first_exec::warm(&candidate); // R9.F2: `on_boot`/`prepare` self-check it on a 20 s clock
    // Exactly what `promote` leaves on the deploy path.
    let _flag = Immutable::set(&candidate);

    let gate = UpdateGate::for_binary(data.clone(), candidate.clone());
    gate.on_boot().await;

    let good = gate.last_good_path();
    let tmp = good.with_extension("copying");
    assert!(
        good.is_file(),
        "the running image is recorded as last-good even though it is immutable"
    );
    assert_eq!(
        std::fs::metadata(&good).unwrap().len(),
        std::fs::metadata(&candidate).unwrap().len(),
        "and it is the running image, not something older left in place"
    );
    assert!(
        !tmp.exists(),
        "and NO temp survives: an immutable orphan here is unremovable and \
         permanently wedges every later boot"
    );
    assert!(
        !is_immutable(&good),
        "the installed copy does not inherit `uchg` either — a last-good the \
         daemon cannot replace is a safety net that freezes"
    );
    assert!(events_in_log(&gate).contains(&"last_good".to_string()));

    // The deploy path keeps its flag: clearing it there would hand every cached
    // `cargo build` in every worktree the operator's running daemon.
    assert!(is_immutable(&candidate), "the SOURCE is left immutable");
}

/// **The self-heal.** The orphan already on the operator's disk is exactly the
/// state that made every later boot fail. A boot clears it.
///
/// Fails against the code this replaces: `fs::copy` cannot open an immutable
/// destination, so it returns `EPERM` before anything else happens and the
/// warning says only "could not copy", naming neither the temp nor the fact that
/// last-good has stopped tracking anything.
#[tokio::test]
async fn a_stale_immutable_temp_from_an_earlier_boot_is_cleared() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let _flags = ClearFlags(dir.path().to_path_buf());
    let candidate = dir.path().join("threads-deployed");
    std::fs::copy(real_daemon_binary(), &candidate).unwrap();
    make_executable(&candidate);
    first_exec::warm(&candidate); // R9.F2: `on_boot`/`prepare` self-check it on a 20 s clock

    // The wedge, reproduced: a previous boot's temp, immutable, in the way.
    //
    // `threads.copying`, NOT `threads.last-good.copying` — `with_extension`
    // REPLACES an extension, and `threads.last-good` has one. That is why the
    // orphan in the operator's store is named `threads.copying`, and getting it
    // wrong here made this test pass against a daemon that had not cleared
    // anything.
    let orphan = data.join("threads.copying");
    std::fs::write(&orphan, vec![b'x'; 4096]).unwrap();
    let _flag = Immutable::set(&orphan);
    assert!(is_immutable(&orphan), "the fixture really is immutable");

    let gate = UpdateGate::for_binary(data.clone(), candidate.clone());
    gate.on_boot().await;

    assert!(!orphan.exists(), "the stale immutable temp is cleared, not stepped around");
    assert!(
        gate.last_good_path().is_file(),
        "and the boot it was blocking completes: last-good tracks the running image again"
    );
}

/// The CONTROL for the two above. The same code path, the same assertions, with
/// no immutable flag anywhere — so a green on those two is known to be about the
/// flag and not about `on_boot` promoting a copy under any conditions at all.
#[tokio::test]
async fn a_flagless_running_image_is_recorded_as_last_good_too() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let candidate = dir.path().join("threads-plain");
    std::fs::copy(real_daemon_binary(), &candidate).unwrap();
    make_executable(&candidate);
    first_exec::warm(&candidate); // R9.F2: `on_boot`/`prepare` self-check it on a 20 s clock
    assert!(!is_immutable(&candidate), "the control's source carries no flags");

    let gate = UpdateGate::for_binary(data.clone(), candidate.clone());
    gate.on_boot().await;

    assert!(gate.last_good_path().is_file());
    assert!(!gate.last_good_path().with_extension("copying").exists());
}

/// A SECOND boot does not re-copy. This is what makes the version identity
/// observable end to end rather than only in the pure decision below: whichever
/// arm settles it — the commit when this build carries one, the bytes when it
/// does not — it must settle it the same way twice.
#[tokio::test]
async fn a_second_boot_of_the_same_image_does_not_reinstall_last_good() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let candidate = dir.path().join("threads-deployed");
    std::fs::copy(real_daemon_binary(), &candidate).unwrap();
    make_executable(&candidate);
    first_exec::warm(&candidate); // R9.F2: `on_boot`/`prepare` self-check it on a 20 s clock

    let gate = UpdateGate::for_binary(data.clone(), candidate.clone());
    gate.on_boot().await;
    let good = gate.last_good_path();
    let first = std::fs::metadata(&good).unwrap();
    let (ino1, mtime1) = {
        use std::os::unix::fs::MetadataExt;
        (first.ino(), first.mtime_nsec())
    };

    let gate2 = UpdateGate::for_binary(data.clone(), candidate.clone());
    gate2.on_boot().await;
    let (ino2, mtime2) = {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(&good).unwrap();
        (m.ino(), m.mtime_nsec())
    };
    assert_eq!(
        (ino1, mtime1),
        (ino2, mtime2),
        "the same version is not reinstalled — the file was not replaced"
    );
}

/// `commit_of` reads a commit off the ARTIFACT, and says nothing when the
/// artifact cannot name one.
///
/// The positive half is conditional ON PURPOSE and the reason is the point: this
/// workspace is routinely dirty, and a dirty tree's commit does not name its
/// bytes, so a test that REQUIRED `Some` here would be a test that fails for
/// being run in the normal state of the repo. What is asserted unconditionally is
/// the thing that must hold either way — that the answer agrees with what the
/// running image says about ITSELF, so the two sides of the comparison in
/// `keep_last_good` cannot be reading different rules.
#[tokio::test]
async fn a_binary_names_its_own_commit_or_says_nothing() {
    let _serial = serial();
    let real = real_daemon_binary();
    assert_eq!(
        update::commit_of(&real).as_deref(),
        threads::image::build_commit(),
        "the commit read off the artifact is the commit compiled into it — if \
         these two ever disagree, `keep_last_good` is comparing two rules"
    );

    // Nothing there, and something there that is not this daemon: both are
    // silence, and neither panics. These are the inputs that reach the fallback
    // arm of the decision on a real machine — an old last-good copy, or none.
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        update::commit_of(&dir.path().join("absent")),
        None,
        "a file that is not there names no commit"
    );
    let garbage = dir.path().join("not-a-daemon");
    std::fs::write(&garbage, vec![b'x'; 4096]).unwrap();
    make_executable(&garbage);
    assert_eq!(
        update::commit_of(&garbage),
        None,
        "and a file that cannot answer `--build-stamp` names no commit either, \
         rather than being reported as a commit-less build of something"
    );
}

/// **THE DECISION, exhaustively** — the part that arbitrates, with no filesystem
/// in it.
///
/// Every combination, because a live machine will not produce them on demand: an
/// image with no commit (compiled before `build.rs` existed), an image from a
/// dirty tree (this workspace, most days), and two images that genuinely
/// disagree. The rule being asserted is **silence decides nothing**.
#[test]
fn the_last_good_decision_is_settled_by_the_commit_and_never_by_silence() {
    use update::{last_good_verdict as v, LastGood::*};

    // ── both sides name a commit: the commit decides, bytes are not consulted ─
    let never = || panic!("the byte identity must not be computed when commits settle it");
    assert_eq!(v(Some("abc123"), Some("abc123"), None, never), AlreadyThis);
    assert_eq!(v(Some("abc123"), Some("def456"), None, never), Promote);
    // Same commit, DIFFERENT bytes — a non-reproducible rebuild. Still the same
    // version, which is the whole substantive consequence of the change: under
    // the old byte identity this re-copied 16 MB for no version change.
    assert_eq!(
        v(Some("abc123"), Some("abc123"), Some("cdhash:aa"), || Some("cdhash:bb".into())),
        AlreadyThis
    );

    // ── either side silent: fall through to the TOTAL byte identity ───────────
    let bytes = |x: &'static str| move || Some(x.to_string());
    assert_eq!(v(None, Some("abc"), Some("cdhash:aa"), bytes("cdhash:aa")), AlreadyThis);
    assert_eq!(v(Some("abc"), None, Some("cdhash:aa"), bytes("cdhash:aa")), AlreadyThis);
    assert_eq!(v(None, None, Some("cdhash:aa"), bytes("cdhash:aa")), AlreadyThis);
    assert_eq!(v(None, None, Some("cdhash:aa"), bytes("cdhash:bb")), Promote);

    // A dirty tree reports `None` on its own side even though a commit exists,
    // so it must not be able to claim identity with a DIFFERENT commit's bytes.
    assert_eq!(v(None, Some("abc"), Some("cdhash:aa"), bytes("cdhash:bb")), Promote);

    // ── and two unreadable images are not thereby "the same" ──────────────────
    // `None == None` would promote nothing, for ever. This is the arm that keeps
    // a missing last-good from reading as an up-to-date one.
    assert_eq!(v(None, None, None, || None), Promote);
    assert_eq!(v(None, None, Some("cdhash:aa"), || None), Promote);
    assert_eq!(v(None, None, None, bytes("cdhash:aa")), Promote);
}

/// The `--self-check` verb the preflight leans on: exit 0, a line naming this
/// daemon, and — because it runs against a binary we do not yet trust — no
/// config read, no data dir, no `~/.threads`.
#[tokio::test]
async fn the_self_check_verb_identifies_the_daemon_without_touching_anything() {
    let _serial = serial();
    let bin = real_daemon_binary();
    let banner = update::self_check(&bin).await.expect("the real daemon self-checks");
    assert!(banner.contains(SELF_CHECK_OK));

    // A binary that is not this daemon fails the check even though it runs.
    let out = std::process::Command::new("/bin/echo")
        .arg(update::SELF_CHECK_FLAG)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(SELF_CHECK_OK),
        "the check is not something any exit-0 program passes"
    );
    assert!(update::self_check(Path::new("/bin/echo")).await.is_err());
}

// ── 3. the quiet condition ───────────────────────────────────────────────

/// **THE PINNING TEST FOR THE QUIET CONDITION. Both halves of it.**
///
/// Operator, verbatim: *"Quiet moment is anytime that the threads process is not
/// doing anything and we should be able to fundamentally just restart the
/// threads process at any time and it resume ownership of the state of
/// affairs."*
///
/// This test REPLACES `a_restart_queued_while_a_turn_runs_waits_for_the_turn_to_
/// finish`, which asserted the opposite and was correct for the contract it was
/// written against. That contract is superseded, on evidence: `turns == 0` is a
/// fleet-wide simultaneous idle, and on 2026-08-17 a pending restart sat behind
/// it for 266 minutes with five threads in a permanent five-minute limit loop.
/// A later agent must not restore the old assertion without deliberately
/// deleting this one, which is the point of writing the reason down here.
///
/// Half one: a live turn is NOT a blocker. It is still REPORTED — `turns` rides
/// on the ledger line, because an operator needs to see what the daemon was
/// carrying — and the turn is not disturbed by the restart: the `running_turns`
/// row that makes it re-adoptable is still on disk afterwards.
///
/// Half two: the spawn window IS a blocker. While a `SpawnPermit` is
/// outstanding the daemon is not quiet, and it says so.
///
/// Forced to fail before it was trusted, both ways:
/// * putting `turns == 0` back into `Blockers::quiet` fails half one at
///   `WindowOutcome::Fired` — it defers instead, forever;
/// * removing the `enter_spawn` permit from `run_once` fails half two, which is
///   the daemon exec'ing over a child nothing on disk names yet.
#[tokio::test]
async fn running_turns_are_not_a_reason_to_wait_and_the_spawn_window_is() {
    let _serial = serial();
    set_env();
    let dir = tempfile::tempdir().unwrap();
    let (mgr, gate) = manager_with_gate(dir.path());
    let log = watch(&mgr);
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));

    let id = mgr.create_thread(None, plain(dir.path())).unwrap();
    let release = dir.path().join("release-the-turn");
    mgr.send(
        &id,
        format!("holdturn please release={}", release.display()),
        SendIntent::Immediate,
        serde_json::Value::Null,
        false,
    )
    .unwrap();
    assert!(eventually(|| mgr.is_locked(&id)).await, "the turn started");
    // The turn is PAST its spawn window — the row that makes it recoverable is
    // written — which is exactly the state in which a restart is safe.
    assert!(
        eventually(|| mgr.store.get_running_turn(&id).unwrap().is_some()).await,
        "the child's identity is on disk, so a restart can re-adopt it"
    );

    // ── HALF TWO, taken first because it needs the turn to be live ────────
    // Stand a spawn permit up by hand: this is the state `run_once` is in
    // between `cmd.spawn()` and `upsert_running_turn`, and it is the ONLY state
    // that cannot survive an `execv`.
    let spawning = gate.enter_spawn();
    gate.request_restart(RestartReason::Operator {
        note: "a new build landed while a turn was running".into(),
    });
    gate.arm().await.expect("the real daemon preflights");
    let outcome = gate.attempt().await.expect("a restart is pending");
    assert_eq!(
        outcome,
        WindowOutcome::Busy(Blockers { turns: 1, spawning: 1, requests: 0 }),
        "a restart must never fire on top of a child that is not on disk yet"
    );
    assert!(!hit.load(Ordering::SeqCst), "nothing was exec'd inside the spawn window");
    assert!(gate.admitting(), "a deferred restart leaves the daemon fully open");
    let deferred: Vec<_> = log_lines(&gate)
        .into_iter()
        .filter(|v| v["event"] == "deferred")
        .collect();
    assert_eq!(deferred.len(), 1, "requirement 7: the log says why it waited");
    assert_eq!(deferred[0]["spawning"], 1, "and names the real blocker");
    assert_eq!(
        deferred[0]["turns"], 1,
        "and REPORTS the running turn, which is not the blocker but is what the operator \
         needs to see"
    );

    // ── HALF ONE: the spawn window closes; the TURN IS STILL RUNNING ──────
    drop(spawning);
    assert!(mgr.is_locked(&id), "the turn is still executing — nothing released it");
    let outcome = gate.attempt().await.expect("still pending");
    assert_eq!(
        outcome,
        WindowOutcome::Fired,
        "a running turn is not a reason to wait: it survives the execv and is re-adopted"
    );
    assert!(hit.load(Ordering::SeqCst), "the exec point was reached ON TOP OF a live turn");
    assert!(events_in_log(&gate).contains(&"firing".to_string()));

    // And what makes that safe is still true afterwards: the turn is running,
    // it holds its lock, and the row `recover()` re-adopts from is intact.
    assert!(mgr.is_locked(&id), "the restart did not touch the execution");
    let row = mgr.store.get_running_turn(&id).unwrap().expect("still on disk");
    assert!(row.pid > 0, "with a pid the next daemon can re-adopt: {row:?}");
    assert_eq!(count_starts(&log), 1, "and no second execution was started for it");

    // Let it finish so the tempdir tears down cleanly.
    gate.reopen_for_test();
    std::fs::write(&release, b"go").unwrap();
    assert!(eventually(|| !mgr.is_locked(&id)).await, "the turn finished on its own");
    assert!(
        log.lock().unwrap().iter().any(|e| matches!(e, ThreadEvent::TurnEnded { .. })),
        "the turn that was running when the restart fired ENDED normally"
    );
}

/// **Waiting on placement does not block quiet.** Operator, verbatim: *"if it's
/// waiting to do placement or something like that, that should not interrupt the
/// quiet moment."*
///
/// A thread that has been PARKED by the admission gate is the durable shape of
/// "waiting to do placement": `admit_or_park`'s only two answers are run-now and
/// a durable row, and a parked row is re-asked by `resume_sweep` on the other
/// side of the restart. So a daemon whose every thread is waiting on the gate is
/// a daemon that is not doing anything, which is precisely a quiet moment.
///
/// Distinct from `parked_turns_and_a_full_queue_do_not_block_a_restart`, which
/// asserts the same thing with no execution anywhere: this one holds a real
/// admission permit while it asks, so it also pins that the WAIT ITSELF — a
/// thread holding a `TurnLock` while parked or floored — is not a blocker.
///
/// Forced to fail before it was trusted: adding `self.turns == 0` back to
/// `Blockers::quiet` turns the `Fired` below into `Busy`.
#[tokio::test]
async fn a_thread_waiting_on_placement_does_not_block_the_quiet_moment() {
    let _serial = serial();
    set_env();
    let dir = tempfile::tempdir().unwrap();
    let (mgr, gate) = manager_with_gate(dir.path());
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));

    let id = mgr.create_thread(None, plain(dir.path())).unwrap();
    // A durable park — the gate said "not now" — plus a retry floor and a queue
    // nobody is draining. Every shape of waiting this daemon has.
    mgr.store
        .upsert_parked_turn(&ParkedTurnRow {
            thread_id: id.clone(),
            prompt: "waiting on placement".into(),
            message_ids: vec![11],
            attempt: 2,
            reason: "no account is currently placeable".into(),
            budget_key: None,
            parked_at: threads::store::now_rfc3339(),
        })
        .unwrap();
    mgr.store
        .enqueue(&id, 12, "queued behind the park", "immediate", &serde_json::Value::Null, &[])
        .unwrap();

    // …and a live execution on top of all of it, holding the admission permit.
    let held = mgr.try_lock_thread(&id, "test-placement-wait").expect("admission is open");
    assert_eq!(gate.peek().turns, 1, "the permit is outstanding and reported");
    assert_eq!(gate.peek().spawning, 0, "nothing is inside a spawn window");

    gate.request_restart(RestartReason::Operator { note: "deploy".into() });
    assert_eq!(
        gate.attempt().await,
        Some(WindowOutcome::Fired),
        "waiting on placement, a parked prompt, a queue and a held turn lock are NOT reasons \
         to wait — every one of them is a durable row `recover()` re-asks"
    );
    assert!(hit.load(Ordering::SeqCst));
    assert!(
        mgr.store.get_parked_turn(&id).unwrap().is_some(),
        "and the parked prompt is exactly where it was"
    );
    drop(held);
}

/// **The check-then-act race, closed.** While the window is open, a task that
/// tries to start a turn cannot get in — not "usually loses the race", cannot.
///
/// The probe thread is released at the START of the exec hook, i.e. after quiet
/// was verified and admission was closed but before the process would be gone —
/// exactly the interleaving a check-then-act bug lives in. It blocks on the same
/// mutex the window holds, and when it finally runs it is REFUSED.
///
/// Fails without the single critical section: if the quiet check and the exec
/// were separate lock acquisitions, the probe would take a permit in the gap and
/// a turn would start under a restart that is already committed.
#[tokio::test]
async fn admission_is_closed_across_the_whole_window_so_no_turn_can_slip_in() {
    let _serial = serial();
    set_env();
    let dir = tempfile::tempdir().unwrap();
    let (mgr, gate) = manager_with_gate(dir.path());
    let id = mgr.create_thread(None, plain(dir.path())).unwrap();

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    gate.set_exec_hook(Box::new(move |_| {
        // We are INSIDE the window: quiet has been verified and admission is
        // closed. Let the probe loose and hold the window open while it tries.
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        Ok(())
    }));

    let probe_mgr = mgr.clone();
    let probe_id = id.clone();
    let probe = std::thread::spawn(move || {
        rx.recv().unwrap();
        probe_mgr.try_lock_thread(&probe_id, "test-probe").err()
    });

    gate.request_restart(RestartReason::Operator { note: "deploy".into() });
    assert_eq!(gate.attempt().await, Some(WindowOutcome::Fired));

    let refusal = probe.join().unwrap();
    assert_eq!(
        refusal.map(|r| r.as_str()),
        Some("restart_pending"),
        "a turn that tried to start inside the window was refused, not admitted"
    );
    assert!(!gate.admitting(), "after firing, admission stays closed — the process is gone");
    assert_eq!(
        gate.peek(),
        Blockers { turns: 0, spawning: 0, requests: 0 },
        "the refused attempt never became a live execution"
    );

    // The refusal loses NOTHING: the send that arrives now is still accepted,
    // still persisted, and still waiting for the daemon that comes back.
    let mid = mgr
        .send(&id, "after the window".into(), SendIntent::Immediate, serde_json::Value::Null, false)
        .unwrap();
    assert!(mid > 0);
    let waiting = mgr.store.waiting_queue(&id).unwrap();
    assert_eq!(waiting.len(), 1, "the message is queued, not dropped and not run");
    assert!(!mgr.is_locked(&id), "and nothing started under a closed admission");
}

/// The window's own check is not a formality behind the cheap peek: called
/// directly, it refuses to open while a BLOCKING permit is outstanding, and once
/// it has fired, admission is shut — `admit_turn` answers `None` from then on.
///
/// This is the invariant stated at its narrowest: quiet, closed, exec — and a
/// turn cannot be admitted after the close. It also states, at that same
/// narrowness, which permits block: a request does, a spawn does, and an
/// admission permit does NOT.
#[tokio::test]
async fn the_window_refuses_while_any_permit_is_outstanding_and_shuts_admission_when_it_fires() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let gate = gate_with_candidate(&data, &dir.path().join("threads-candidate"));

    let serving = gate.enter_request();
    assert_eq!(
        gate.with_quiet_window(|| panic!("the window must not open while a request is served")),
        WindowOutcome::Busy(Blockers { turns: 0, spawning: 0, requests: 1 })
    );
    drop(serving);

    let spawning = gate.enter_spawn();
    assert_eq!(
        gate.with_quiet_window(|| panic!("the window must not open inside a spawn window")),
        WindowOutcome::Busy(Blockers { turns: 0, spawning: 1, requests: 0 })
    );
    assert!(gate.admitting(), "a refused window changes nothing");
    drop(spawning);

    // An admission permit is REPORTED and does not refuse the window. A running
    // turn survives the exec; see `Blockers::quiet`.
    let permit = gate.admit_turn().expect("admission is open");
    assert_eq!(gate.peek().turns, 1, "it is counted");
    assert_eq!(gate.with_quiet_window(|| Ok(())), WindowOutcome::Fired);
    drop(permit);
    assert!(
        gate.admit_turn().is_none(),
        "once the window has fired, no turn is ever admitted again — in production \
         this process no longer exists"
    );

    // A window whose act FAILS puts everything back exactly as it was.
    gate.reopen_for_test();
    assert_eq!(
        gate.with_quiet_window(|| Err("nope".into())),
        WindowOutcome::Refused("nope".into())
    );
    assert!(gate.admitting(), "a failed exec leaves the old daemon serving");
    assert!(gate.admit_turn().is_some());
}

/// **Requirement 3, the WS half.** A request being served is a reason to wait,
/// and finishing it is a reason to go.
#[tokio::test]
async fn an_in_flight_request_defers_the_restart_and_finishing_it_releases_it() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let gate = gate_with_candidate(&data, &dir.path().join("threads-candidate"));
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));
    gate.request_restart(RestartReason::Operator { note: "deploy".into() });

    let serving = gate.enter_request();
    assert_eq!(
        gate.attempt().await,
        Some(WindowOutcome::Busy(Blockers { turns: 0, spawning: 0, requests: 1 }))
    );
    assert!(!hit.load(Ordering::SeqCst));

    drop(serving);
    assert_eq!(gate.attempt().await, Some(WindowOutcome::Fired));
    assert!(hit.load(Ordering::SeqCst));
}

// ── 3b. D-28: the quiet moment is HELD, not chased ───────────────────────

/// **D-28's starvation, closed at the root instead of at the edge.** A restart
/// pending behind N running turns fires PROMPTLY — while they are still running,
/// without waiting for a single one of them to end, and without a timer tick.
///
/// This test's ancestor
/// (`a_restart_fires_at_a_turn_completion_not_after_the_backlog_drains`)
/// asserted the best that could be done while `turns` was still in the quiet
/// condition: fire at the zero-crossing between two queued turns, because that
/// gap is microseconds long and had to be HELD rather than chased. That fix was
/// right and it was not enough — a zero-crossing that never comes cannot be
/// held, and with five threads in a permanent five-minute limit loop the
/// counter never reached zero for 266 minutes. So the condition itself moved.
///
/// The poll floor here is an HOUR, so a timer tick cannot rescue this test.
///
/// Fails with `turns` restored to `Blockers::quiet`: the first turn is held open
/// by this test for the whole run, so nothing ever reaches a zero-crossing and
/// `hit` stays false until `eventually` gives up.
#[tokio::test]
async fn a_restart_pending_behind_running_turns_fires_promptly() {
    let _serial = serial();
    set_env();
    std::env::set_var("THREADS_UPDATE_POLL_MS", "3600000");
    let dir = tempfile::tempdir().unwrap();
    let (mgr, gate) = manager_with_watchable_gate(dir.path());
    let log = watch(&mgr);
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));

    // THREE THREADS, each with a turn that never ends on its own — the shape
    // that starved. `turns` will sit at 3 for the whole test.
    let mut ids = Vec::new();
    let release = dir.path().join("release-the-turns");
    for _ in 0..3 {
        let id = mgr.create_thread(None, plain(dir.path())).unwrap();
        mgr.send(
            &id,
            format!("holdturn release={}", release.display()),
            SendIntent::Immediate,
            serde_json::Value::Null,
            false,
        )
        .unwrap();
        ids.push(id);
    }
    assert!(
        eventually(|| ids.iter().all(|i| mgr.is_locked(i))).await,
        "all three turns started"
    );
    assert!(
        eventually(|| gate.peek().turns == 3).await,
        "and all three are counted: {:?}",
        gate.peek()
    );

    gate.request_restart(RestartReason::Operator {
        note: "a new build landed while three turns were running".into(),
    });
    gate.arm().await.expect("the real daemon preflights");
    assert!(gate.armed().is_some(), "the candidate is staged");
    gate.spawn_watcher();

    assert!(
        eventually(|| hit.load(Ordering::SeqCst)).await,
        "the restart fired with three turns still running and an hour until the next tick"
    );
    // NOTHING WAS RELEASED. The three turns are exactly as they were.
    assert!(ids.iter().all(|i| mgr.is_locked(i)), "the restart did not end any of them");
    assert_eq!(gate.peek().turns, 3, "and none of them handed its permit back");
    assert_eq!(count_starts(&log), 3, "no extra execution was started either");
    // And every one of them is re-adoptable on the other side, which is what
    // makes firing over them safe rather than merely fast.
    for id in &ids {
        let row = mgr.store.get_running_turn(id).unwrap().expect("still on disk");
        assert!(row.pid > 0, "{id} can be re-adopted from {row:?}");
        assert!(row.prompt.is_some(), "and the adopter can read the prompt back");
    }
    assert!(events_in_log(&gate).contains(&"firing".to_string()));

    gate.reopen_for_test();
    std::fs::write(&release, b"go").unwrap();
    assert!(eventually(|| ids.iter().all(|i| !mgr.is_locked(i))).await, "they all finished");
    std::env::remove_var("THREADS_UPDATE_POLL_MS");
}

/// The hold is what makes "nothing is being started" true, and it is taken by
/// the RELEASE itself — not by a watcher that has to notice. And a hold that
/// nobody ever redeems heals, because a daemon may refuse turns while it is
/// about to restart but must never refuse them forever.
///
/// Fails without the self-heal: a watcher that dies between the hold and the
/// exec wedges the daemon shut permanently, which is strictly worse than the
/// starvation it was built to fix.
#[tokio::test]
async fn the_release_itself_closes_admission_and_an_unredeemed_hold_heals() {
    let _serial = serial();
    std::env::set_var("THREADS_RESTART_HOLD_MS", "150");
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let gate = gate_with_candidate(&data, &dir.path().join("threads-candidate"));
    gate.request_restart(RestartReason::Operator { note: "deploy".into() });

    // Nothing is held until something is STAGED: closing admission with no
    // approved binary to exec would be a stall, not a window.
    let spawning = gate.enter_spawn();
    assert!(!gate.window_held(), "no candidate is staged yet");
    drop(spawning);
    assert!(gate.admitting(), "a zero-crossing with nothing armed changes nothing");

    gate.arm().await.expect("the real daemon preflights");
    // The zero-crossing that matters is the SPAWN window's, not a turn's — a
    // running turn is not a blocker (`Blockers::quiet`), so its release is not
    // the crossing this mechanism is built around.
    let spawning = gate.enter_spawn();
    assert!(!gate.window_held(), "a turn inside its spawn window is not a quiet moment");

    drop(spawning); // ── THE ZERO-CROSSING ──
    assert!(gate.window_held(), "the release itself closed the window, synchronously");
    assert!(!gate.admitting());
    assert!(
        gate.admit_turn().is_none(),
        "a turn that asks inside the held window is refused — this is the whole point"
    );
    assert!(events_in_log(&gate).contains(&"window_held".to_string()));

    // Nobody redeems it. It must not wedge the daemon shut.
    tokio::time::sleep(Duration::from_millis(220)).await;
    let recovered = gate
        .admit_turn()
        .expect("a hold nobody redeemed reopens rather than refusing turns forever");
    assert!(events_in_log(&gate).contains(&"hold_expired".to_string()));
    drop(recovered);
    std::env::remove_var("THREADS_RESTART_HOLD_MS");
}

/// A held window broken by a WS request reopens instead of staying shut. A
/// client is never made to wait on a restart (`enter_request` is never
/// refused), so this interleaving is reachable and must be survivable.
#[tokio::test]
async fn a_request_that_breaks_a_held_window_reopens_the_daemon() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let gate = gate_with_candidate(&data, &dir.path().join("threads-candidate"));
    gate.request_restart(RestartReason::Operator { note: "deploy".into() });
    gate.arm().await.expect("the real daemon preflights");

    let permit = gate.admit_turn().unwrap();
    let serving = gate.enter_request(); // never refused, even while we would like to restart
    drop(permit);
    assert!(!gate.window_held(), "a request in flight is not a quiet moment");
    assert!(gate.admitting());

    drop(serving); // now the request's release is the zero-crossing
    assert!(gate.window_held());

    // A request arrives inside the held window and breaks it.
    let late = gate.enter_request();
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));
    assert_eq!(
        gate.fire_held(),
        Some(WindowOutcome::Busy(Blockers { turns: 0, spawning: 0, requests: 1 }))
    );
    assert!(!hit.load(Ordering::SeqCst), "nothing was exec'd on top of a live request");
    assert!(gate.admitting(), "and the daemon went straight back to serving");
    assert!(!gate.window_held());
    drop(late);
    assert!(gate.window_held(), "the next zero-crossing re-holds it");
    assert_eq!(gate.fire_held(), Some(WindowOutcome::Fired));
    assert!(hit.load(Ordering::SeqCst));
}

/// **THE STARVATION BOUND.** A restart still pending past the threshold stops
/// being a quiet `deferred` line and starts shouting, with its age and its
/// blockers — and does not flood.
///
/// Fails without `check_starvation`: this is exactly the hour of silence that
/// hid D-28, where the only evidence was a human comparing a running pid
/// against a file on disk.
#[tokio::test]
async fn a_restart_that_has_been_pending_too_long_says_so_loudly() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let candidate = dir.path().join("threads-candidate");
    let gate = gate_with_candidate(&data, &candidate);

    // A restart queued ninety minutes ago and never performed — written the way
    // a daemon that died while deferring would have left it, then adopted at
    // boot. The clock is the DURABLE one, so it survives the exit too.
    let stale = threads::update::PendingRestart {
        queued_at: (chrono::Utc::now() - chrono::Duration::minutes(90)).to_rfc3339(),
        kind: "new_binary".into(),
        identity: Some("sha256:a-build-we-never-got-onto".into()),
        note: "a new daemon binary is on disk".into(),
        binary: candidate.display().to_string(),
    };
    std::fs::write(gate.pending_path(), serde_json::to_string_pretty(&stale).unwrap()).unwrap();
    let gate = UpdateGate::for_binary(data.clone(), candidate.clone());
    gate.on_boot().await;
    assert!(gate.pending().is_some(), "a restart we never performed is still owed");

    // Busy. The blocker is the SPAWN window — the only thing a restart waits
    // for — and a turn permit is held alongside it so this also pins that
    // `turns` is still REPORTED on the escalation line even though it is not
    // what the daemon is starving behind.
    let _turn = gate.admit_turn().expect("admission is open");
    let _spawning = gate.enter_spawn();
    gate.attempt().await;

    let starving: Vec<_> = log_lines(&gate)
        .into_iter()
        .filter(|v| v["event"] == "starving")
        .collect();
    assert_eq!(starving.len(), 1, "a starving restart says so: {starving:?}");
    assert!(
        starving[0]["pending_secs"].as_u64().unwrap() >= 89 * 60,
        "and says how long it has been starving: {:?}",
        starving[0]
    );
    assert_eq!(starving[0]["spawning"], 1, "and names what it is starving behind");
    assert_eq!(
        starving[0]["turns"], 1,
        "and still REPORTS the running turn — the operator needs to see what the daemon was \
         carrying, even though a running turn is not what a restart waits for"
    );

    // Loud, not deafening.
    for _ in 0..5 {
        gate.attempt().await;
    }
    assert_eq!(
        log_lines(&gate).iter().filter(|v| v["event"] == "starving").count(),
        1,
        "the escalation repeats on an interval, it does not flood every tick"
    );
}

/// **THE SPAWN WINDOW IS TAKEN WHERE IT HAS TO BE TAKEN.**
///
/// The behavioural tests above stand a `SpawnPermit` up by hand, because the
/// real one is alive for microseconds — between `cmd.spawn()` and the
/// `upsert_running_turn` that makes the child recoverable — and no test can
/// reliably observe a window that short from outside. So the placement itself is
/// pinned here, in the same shape AUI uses for its own removed wait
/// (`apps/aui/src/restart.rs::the_executor_no_longer_asks_what_the_agents_are_
/// doing`): read the source and assert the ORDER of three lines.
///
/// That order is the whole invariant. A permit taken after `spawn()`, or
/// released before the row is written, leaves exactly the gap this exists to
/// close: an `execv` on top of a live `claude` process that nothing on disk
/// names, which `recover()` can neither re-adopt nor bury.
///
/// Forced to fail before it was trusted, three ways: deleting the `enter_spawn`
/// line, moving it below `cmd.spawn()`, and moving `spawn_permit.release()`
/// above the `upsert_running_turn` call each turn this red.
#[test]
fn the_spawn_permit_wraps_exactly_the_unrecoverable_window() {
    let _serial = serial();
    let src = include_str!("../../src/runtime.rs");
    let body = src
        .split_once("async fn run_once(")
        .expect("run_once exists")
        .1;
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is gone from run_once — the spawn window is unguarded"))
    };
    let take = at("self.update.enter_spawn()");
    let spawn = at("let mut child = match cmd.spawn()");
    let record = at("upsert_running_turn(");
    let release = at("spawn_permit.release()");
    assert!(
        take < spawn,
        "the spawn permit must be taken BEFORE the child exists, or a restart can land \
         between the two"
    );
    assert!(
        record < release,
        "the spawn permit must be held UNTIL the child's identity is on disk, or a restart \
         can land on a child `recover()` will never see"
    );
    assert!(spawn < record, "the row is written after the spawn, as it always was");
}

/// **`turns` IS NOT IN THE QUIET PREDICATE, AND IT IS STILL REPORTED.**
///
/// The matched pair of the test above, on the other side of the change. A later
/// agent restoring the wait-for-turns condition has to delete this test on
/// purpose, and to read why it exists while doing so.
///
/// Operator, verbatim: *"Quiet moment is anytime that the threads process is not
/// doing anything and we should be able to fundamentally just restart the
/// threads process at any time and it resume ownership of the state of
/// affairs."*
///
/// Forced to fail before it was trusted: restoring `self.turns == 0 &&` to
/// `Blockers::quiet` fails the first assertion; deleting the `turns` field
/// fails the second and third.
#[test]
fn the_quiet_predicate_reads_the_spawn_window_and_not_the_running_turns() {
    let _serial = serial();
    let src = include_str!("../../src/update.rs");
    let predicate = src
        .split_once("pub fn quiet(&self) -> bool {")
        .expect("Blockers::quiet exists")
        .1
        .split_once('}')
        .expect("its body closes")
        .0;
    assert!(
        !predicate.contains("turns"),
        "the wait-for-turns condition is back in `Blockers::quiet`: {predicate:?}. A running \
         turn survives the execv (own process group, file-backed stdout, durable \
         `running_turns` row, `recover()` -> `adopt_turn`); waiting for every turn in the \
         fleet to end at the same instant starved a restart for 266 minutes."
    );
    assert!(
        predicate.contains("self.spawning == 0") && predicate.contains("self.requests == 0"),
        "the quiet condition must still be the spawn window and in-flight requests: \
         {predicate:?}"
    );
    // REPORTED, not tested. Both halves matter: the field exists, and the
    // ledger lines carry it.
    assert!(
        src.contains("pub turns: usize"),
        "`turns` must survive as a reported field — an operator reading a `deferred` or \
         `starving` line needs to see what the daemon was carrying"
    );
    // Anchored inside `impl Blockers` on purpose — `RestartReason` has a
    // `describe` of its own, and it appears first in the file.
    let describe = src
        .split_once("impl Blockers {")
        .expect("impl Blockers exists")
        .1
        .split_once("pub fn describe(&self) -> String {")
        .expect("Blockers::describe exists")
        .1;
    assert!(
        describe[..400].contains("self.turns"),
        "and `Blockers::describe` must still say it: {:?}",
        &describe[..400.min(describe.len())]
    );
}

// ── 4. what must NOT block, and what must survive ────────────────────────

/// **PARKED TURNS DO NOT BLOCK A RESTART. This is deliberate.**
///
/// A parked turn is a durable row that exists because something said "not now",
/// and it can sit there until the operator raises a limit. Waiting queue rows and
/// armed retries are the same shape. If any of them were part of the quiet
/// condition, this daemon — whose steady state is a backlog — would never restart
/// at all, which is the failure mode this whole mechanism exists to end.
///
/// Fails the day somebody adds "and the queue is empty" to the quiet condition.
#[tokio::test]
async fn parked_turns_and_a_full_queue_do_not_block_a_restart() {
    let _serial = serial();
    set_env();
    let dir = tempfile::tempdir().unwrap();
    let (mgr, gate) = manager_with_gate(dir.path());
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));

    let id = mgr.create_thread(None, plain(dir.path())).unwrap();
    // A durable parked turn, with its prompt.
    mgr.store
        .upsert_parked_turn(&ParkedTurnRow {
            thread_id: id.clone(),
            prompt: "the parked prompt, verbatim".into(),
            message_ids: vec![7],
            attempt: 2,
            reason: "budget: over the session limit".into(),
            budget_key: Some("ops".into()),
            parked_at: String::new(),
        })
        .unwrap();
    // And a queue nobody is draining.
    mgr.store.enqueue(&id, 8, "queued behind the park", "immediate", &serde_json::Value::Null, &[]).unwrap();
    mgr.store.enqueue(&id, 9, "and another", "turn_end", &serde_json::Value::Null, &[]).unwrap();

    assert!(!mgr.store.waiting_queue(&id).unwrap().is_empty());
    assert!(mgr.store.get_parked_turn(&id).unwrap().is_some());

    gate.request_restart(RestartReason::Operator { note: "deploy".into() });
    assert_eq!(
        gate.attempt().await,
        Some(WindowOutcome::Fired),
        "a parked turn and a full queue are NOT reasons to wait — they are durable"
    );
    assert!(hit.load(Ordering::SeqCst));
}

/// **Requirement 6, provably.** Everything durable is exactly where it was on the
/// other side of the restart, and a freshly booted daemon over the same data dir
/// sees all of it.
#[tokio::test]
async fn nothing_durable_is_lost_across_the_restart() {
    let _serial = serial();
    set_env();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let (mgr, gate) = manager_with_gate(dir.path());
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));

    let id = mgr.create_thread(None, plain(dir.path())).unwrap();
    // One real (fake-claude) turn, so the ledger has history to lose.
    let mid = mgr
        .send(&id, "a message that must survive".into(), SendIntent::TurnEnd, serde_json::Value::Null, false)
        .unwrap();
    assert!(eventually(|| mgr.is_locked(&id)).await);
    assert!(eventually(|| !mgr.is_locked(&id)).await, "the turn finished");

    // Now the durable backlog: a parked prompt and two queue rows behind it.
    mgr.store
        .upsert_parked_turn(&ParkedTurnRow {
            thread_id: id.clone(),
            prompt: "the parked prompt, verbatim".into(),
            message_ids: vec![mid],
            attempt: 3,
            reason: "budget: over the session limit".into(),
            budget_key: Some("ops".into()),
            parked_at: String::new(),
        })
        .unwrap();
    mgr.store.enqueue(&id, mid + 1, "queued behind the park", "immediate", &serde_json::Value::Null, &[]).unwrap();
    mgr.store.enqueue(&id, mid + 2, "and another", "turn_end", &serde_json::Value::Null, &[]).unwrap();
    mgr.store.mark_seen(&id, 1).unwrap();
    let seq_before = mgr.store.max_seq(&id).unwrap();
    let queued_before = mgr.store.waiting_queue(&id).unwrap();
    assert_eq!(queued_before.len(), 2);

    gate.request_restart(RestartReason::Operator { note: "deploy".into() });
    assert_eq!(gate.attempt().await, Some(WindowOutcome::Fired));
    assert!(hit.load(Ordering::SeqCst));
    drop(mgr);

    // THE COME-BACK. A new daemon process over the same data dir.
    let reborn = build_manager(config_at(&data))
        .unwrap()
        .with_update_gate(UpdateGate::for_binary(
            data.clone(),
            dir.path().join("threads-candidate"),
        ));

    let parked = reborn.store.get_parked_turn(&id).unwrap().expect("the parked prompt survived");
    assert_eq!(parked.prompt, "the parked prompt, verbatim");
    assert_eq!(parked.message_ids, vec![mid]);
    assert_eq!(parked.attempt, 3);
    assert_eq!(
        reborn.store.waiting_queue(&id).unwrap().len(),
        queued_before.len(),
        "the queue rows are still waiting — nothing was marked delivered"
    );
    assert_eq!(reborn.store.max_seq(&id).unwrap(), seq_before, "the ledger is intact");
    assert_eq!(reborn.store.last_seen_seq(&id).unwrap(), 1, "the unread watermark is intact");
    assert!(reborn.store.get_thread(&id).unwrap().is_some(), "the thread record is intact");
    let status = reborn.thread_status(&id).unwrap();
    assert_eq!(status["state"], "parked");
    assert_eq!(status["executing"], false);
}

// ── 5. the log ───────────────────────────────────────────────────────────

/// **Requirement 7.** Every decision is legible afterwards, and a long defer is
/// a handful of lines rather than one per tick.
#[tokio::test]
async fn the_decision_log_records_every_defer_and_the_fire_without_flooding() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let gate = gate_with_candidate(&data, &dir.path().join("threads-candidate"));
    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));
    gate.request_restart(RestartReason::Operator { note: "deploy".into() });

    let serving = gate.enter_request();
    for _ in 0..20 {
        gate.attempt().await;
    }
    drop(serving);
    assert_eq!(gate.attempt().await, Some(WindowOutcome::Fired));

    let deferred: Vec<_> = log_lines(&gate)
        .into_iter()
        .filter(|v| v["event"] == "deferred")
        .collect();
    assert_eq!(
        deferred.len(),
        1,
        "twenty identical defers are one line, not twenty: {deferred:?}"
    );
    assert_eq!(deferred[0]["requests"], 1, "and it names what it was waiting on");
    let events = events_in_log(&gate);
    assert!(events.contains(&"queued".to_string()));
    assert!(events.contains(&"preflight".to_string()));
    assert!(events.contains(&"firing".to_string()));
}

/// The watcher never arms for anything that is not the daemon binary — so it can
/// never fire inside a `cargo test` process, and the `execv` cannot be reached
/// by a harness that happens to look busy.
#[tokio::test]
async fn the_watcher_never_arms_for_a_binary_that_is_not_the_daemon() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let gate = gate_with_candidate(&data, &dir.path().join("threads-candidate"));
    assert!(!gate.watch_wanted(), "`threads-candidate` is not `threads`");

    // The gate this very test process builds by default is disarmed outright.
    let default_gate = UpdateGate::new(data.clone());
    assert!(default_gate.boot_identity().is_none());
    assert!(!default_gate.watch_wanted());
    assert!(default_gate.prepare().await.is_err(), "a disarmed gate cannot preflight anything");

    // Named `threads`, it arms.
    let armed_dir = dir.path().join("bin");
    std::fs::create_dir_all(&armed_dir).unwrap();
    let armed = gate_with_candidate(&data, &armed_dir.join("threads"));
    assert!(armed.watch_wanted());
    std::env::set_var("THREADS_UPDATE_WATCH", "off");
    assert!(!armed.watch_wanted(), "and the off switch works");
    std::env::remove_var("THREADS_UPDATE_WATCH");
}

/// A build that lands BETWEEN the preflight and the window aborts the window
/// rather than exec'ing something nobody verified.
#[tokio::test]
async fn a_binary_that_changes_after_preflight_aborts_the_window() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let candidate = dir.path().join("threads-candidate");
    let gate = gate_with_candidate(&data, &candidate);
    gate.request_restart(RestartReason::Operator { note: "deploy".into() });

    let ready = gate.prepare().await.expect("the real daemon preflights");
    // Another build lands. A re-link, not a byte-poke: `codesign -dvvv` reports
    // the STORED code directory rather than recomputing page hashes, so
    // appending bytes to a signed Mach-O leaves the code identity untouched and
    // would make this test pass for no reason. The ad-hoc re-sign reproduces
    // what a real relink does — a fresh code directory, a new CDHash.
    let before = update::binary_identity(&candidate).expect("the candidate fingerprints");
    fake_unsigned_rebuild(&candidate, "newer-still");
    make_executable(&candidate);
    assert_ne!(
        update::binary_identity(&candidate).as_deref(),
        Some(before.as_str()),
        "the fixture must actually change the code identity, or this proves nothing"
    );

    let hit = Arc::new(AtomicBool::new(false));
    gate.set_exec_hook(recording_hook(hit.clone()));
    match gate.fire(&ready) {
        WindowOutcome::Refused(why) => {
            assert!(why.contains("changed between preflight and the quiet window"), "{why}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(!hit.load(Ordering::SeqCst), "nothing unverified was exec'd");
    assert!(gate.admitting(), "and the daemon went straight back to serving");
    assert_eq!(
        LockRefusal::RestartPending.as_str(),
        "restart_pending",
        "the refusal a turn would have seen has a name in the log"
    );
}
