//! **The daemon's own update protocol — watch the binary, restart at the first
//! true quiet moment.**
//!
//! Operator, msg #1630 (intent item 64):
//!
//! > "How are you gonna restart it if you're running on it and you're required
//! >  to not be running? You need to put in an update protocol. You need it to
//! >  look for its own binary and to restart in that first moment where nothing
//! >  is running and nothing is being started."
//!
//! This is a DEADLOCK, not an inconvenience. Every agent that could deploy this
//! daemon is hosted BY this daemon, so the restart kills the turn issuing it.
//! The only actor that can restart the daemon without losing the turn that
//! ordered the restart is the daemon itself, at a moment when there is no turn.
//!
//! # The shape (stolen from `apps/aui/src/restart.rs` + `recover.rs`)
//!
//! binary-identity watch → queue a restart with a reason → execute only at a
//! VERIFIED quiet window → preflight what you are about to become → keep a
//! last-good copy. What is different here is the quiet condition and the
//! come-back.
//!
//! # THE QUIET CONDITION, exactly as implemented
//!
//! **A QUIET MOMENT IS NOT A FLEET-WIDE IDLE.** Operator, verbatim:
//!
//! > "technically it's not doing anything in between […] even while the
//! >  sub-processes are running. So we could like just keep the sub-processes
//! >  running and just have their PID stored in the threads database. And then
//! >  threads can just resume and just check from the last checked date when it
//! >  comes online. And then retry placement and all of that stuff. But if it's
//! >  waiting to do placement or something like that, that should not interrupt
//! >  the quiet moment. **Quiet moment is anytime that the threads process is
//! >  not doing anything and we should be able to fundamentally just restart the
//! >  threads process at any time and it resume ownership of the state of
//! >  affairs.**"
//!
//! Quiet means all three at once, measured as two counters:
//!
//! * `spawning == 0` — no turn is inside the ONE window that cannot survive an
//!   `execv`: between `cmd.spawn()` and the `upsert_running_turn` that records
//!   the child's `(pid, cookie, path, offset)`. A child spawned but not yet
//!   recorded is unrecoverable — nothing on disk names it, so `recover()` can
//!   neither re-adopt it nor bury it, and it runs on as an orphan forever. That
//!   window is microseconds wide and it is the whole of what a restart must
//!   wait for. `run_once` takes a [`SpawnPermit`] across exactly it.
//! * `requests == 0` — no WS request is being served. `server::dispatch` takes a
//!   [`RequestPermit`] for the whole life of every request.
//! * admission is CLOSED for the entire window — see below.
//!
//! ## `turns` IS REPORTED, AND IS NOT A BLOCKER — do not put it back
//!
//! [`Blockers::turns`] still counts outstanding [`TurnPermit`]s and still rides
//! on every `deferred`/`starving` line, because an operator reading the ledger
//! needs to see what the daemon was carrying when it restarted. It is NOT in
//! [`Blockers::quiet`], and restoring it there is the bug this section exists to
//! prevent. Measured, on this machine:
//!
//! * `turns == 0` is a FLEET-WIDE SIMULTANEOUS IDLE — a rendezvous, not a quiet
//!   moment. A `TurnPermit` used to be taken by `lock_turn` as its first act and
//!   held inside the `TurnLock` for the entire execution, so with N concurrent
//!   turns the counter only reaches zero if every one of them happens to be
//!   between turns at the same instant.
//! * On 2026-08-17 a pending restart sat for **266 minutes** and never fired.
//!   Every `starving` line in the ledger reads `requests=0` and `turns` ∈
//!   {1,2,3}: five threads were in a permanent five-minute usage-limit retry
//!   loop, so the counter never reached zero and never would have.
//!
//! ## WHY IT IS SAFE TO STOP WAITING (proven, not assumed)
//!
//! Restarting on top of a running turn does not kill it and does not lose it:
//!
//! * The child leads its OWN process group (`cmd.process_group(0)`), so nothing
//!   aimed at the daemon's group reaches it, and nothing in this crate kills
//!   children on shutdown.
//! * Its stdout is a FILE, not a pipe, so the writer does not care that the
//!   reader went away and nothing is lost while no one is tailing.
//! * Its identity is persisted in `running_turns` immediately after spawn, and
//!   `execv` does not touch child processes at all.
//! * Boot recovery re-adopts it: `recover()` → `adopt_turn`, tailing from the
//!   persisted `byte_offset`. Verified live — 2026-08-13T20:07:52 reclaimed a
//!   stale lock, re-adopted pid 13310, and that turn ran for another day.
//!   Subagents survive identically, for the same three reasons.
//!
//! AUI made exactly this change for exactly this reason (`apps/aui/src/
//! restart.rs`, DIR-54: *"with several long-running threads the quiet moment
//! never came, and the interface ran stale code all day"*) and pinned the
//! removal with a test. So does this module — see
//! `tests/self_update.rs::running_turns_are_not_a_reason_to_wait…`.
//!
//! ## How the check-then-act race is closed
//!
//! It is not closed by ordering, or by a short window, or by re-checking. The
//! check, the closure of admission, and the exec are **one critical section of
//! one mutex** — [`UpdateGate::with_quiet_window`] holds `inner` across all
//! three. `admit_turn` and `enter_request` need that same mutex, so a task that
//! wants to start a turn during the window is not racing, it is BLOCKED on the
//! lock; by the time it acquires it, either this process has been replaced by
//! `execv` (it never acquires anything) or the window failed and admission has
//! already been reopened, and it proceeds normally. There is no interleaving in
//! which a turn starts between the quiet check and the exec, because there is no
//! instruction between them that does not hold the lock.
//!
//! # ARM WHILE BUSY, FIRE AT THE ZERO-CROSSING (the starvation fix, D-28)
//!
//! The version of this module that shipped first was correct and never
//! deployed anything. It deferred against "1 turn(s) executing" for an hour at
//! a time, and the measured reason was NOT that the quiet moment failed to
//! occur — it was that the quiet moment is **microseconds long and nothing was
//! standing at it**:
//!
//! * `runtime::finish_turn` hands the permit back (`lock.release()`) and then,
//!   in the SAME synchronous stretch of the same function with no `.await`
//!   between them, calls `maybe_dispatch`, which takes the next permit. The
//!   counter goes 1 → 0 → 1 without ever yielding to the executor.
//! * The watcher was a SEPARATE tokio task. `wake.notify_one()` handed it a
//!   permit, but it still had to be scheduled, and then it re-ran the whole
//!   expensive [`prepare`](UpdateGate::prepare) — two subprocess `--self-check`
//!   spawns and a SHA-256 of a 27 MB binary — before it reached the window. By
//!   then the next turn had been executing for a long time.
//!
//! The evidence is in `restarts.jsonl`: a `preflight` line with `on_disk_ok:
//! true` and no `firing` after it. The watcher DID observe quiet, DID approve
//! the binary, and then found the daemon busy again when it finally asked for
//! the window. It had spent the quiet moment doing the preflight.
//!
//! So the mechanism is inverted:
//!
//! 1. **Arm while busy.** Everything expensive — preflight, last-good restore,
//!    the identity — happens the moment a restart is queued, WITHOUT waiting for
//!    quiet, and the approved [`Ready`] is staged in `armed`. A busy daemon is
//!    the perfect time to do this; there is nothing else the update wants.
//! 2. **Fire at the zero-crossing, in the release path itself.** When a permit
//!    is handed back, the decrement and the decision are ONE critical section:
//!    [`UpdateGate::consider_hold`] runs under the same `inner` guard that just
//!    decremented the counter, and if that decrement reached zero with a
//!    candidate staged, it CLOSES ADMISSION right there. `maybe_dispatch`'s
//!    `lock_turn`, a few instructions later in `finish_turn`, blocks on that
//!    same mutex and is then refused. **The quiet moment is no longer something
//!    the watcher has to catch — it is held open for it.**
//! 3. The watcher then does the one cheap thing left: re-verify the identity
//!    and `execv`. The held window lasts about as long as one SHA-256 of the
//!    binary — tens of milliseconds — during which turns are refused and their
//!    work stays queued exactly as [`WindowOutcome`] already guaranteed.
//!
//! A held window that somehow never fires is self-healing: `admit_turn` reopens
//! it after [`hold_limit`] and says so at ERROR. A daemon can refuse turns
//! because it is about to restart; it must never refuse them forever.
//!
//! # THE STARVATION BOUND
//!
//! A restart pending longer than `THREADS_RESTART_ESCALATE_SECS` (default 10
//! minutes) is no longer waiting, it is starving, and it says so at ERROR with a
//! `starving` line in the ledger, repeating every 5 minutes. Deferral logging is
//! also re-emitted every 5 minutes rather than deduplicated forever — the
//! original dedup is exactly why an hour of starvation left ONE line in the
//! ledger and looked like seven quiet deferrals.
//!
//! `maybe_dispatch` also peeks at `admitting` before doing any work, but that is
//! a COURTESY that saves a pointless admission evaluation — it is not the gate,
//! and removing it would not create a race. `lock_turn`'s permit is the gate.
//!
//! ## WAITING DOES NOT BLOCK A RESTART — this is deliberate
//!
//! Operator, verbatim: *"if it's waiting to do placement or something like
//! that, that should not interrupt the quiet moment."* A thread waiting on
//! placement, on admission, on a budget or on a retry floor is a daemon that is
//! **not doing anything**, and a daemon that is not doing anything is exactly
//! what a quiet moment is. Every one of those waits is a durable row that
//! `recover()` re-asks on the other side, so restarting through one costs
//! nothing and loses nothing.
//!
//! A later reader will be tempted to add "and no parked turns" and "and the
//! queue is empty" to the quiet condition. **Do not.** That change turns this
//! into a mechanism that never restarts:
//!
//! * A parked turn is a DURABLE row in `parked_turns`. It exists precisely
//!   because something (a budget, a threshold, an account limit) said "not now",
//!   and it can sit there for hours or until the operator raises a limit. It is
//!   designed to outlive daemon restarts — `recover()` re-dispatches it.
//! * Waiting `queue` rows are equally durable, are never marked delivered until
//!   AFTER the turn lock is taken, and are re-dispatched by `recover()`'s
//!   `resume_sweep()`. A dispatch that is refused because admission is closed
//!   leaves every row exactly where it was.
//! * Armed retries are persisted `retries` rows with deadlines, re-armed at boot.
//! * A turn parked or floored by `admit_or_park` — "waiting to do placement" —
//!   is one of the three above by construction: the gate's only two answers are
//!   run-now and a durable row.
//!
//! So "nothing is being started" is enforced by CLOSING admission, not by
//! waiting for the backlog to be empty. A backlog is the normal steady state of
//! this daemon; waiting for it to empty is waiting forever.
//!
//! # THE ABSORB SIGNAL: A SIGNATURE, NOT A BUILD
//!
//! **Checking that it compiles must be a no-op.** Operator, verbatim:
//!
//! > "In terms of who builds the binary, whoever affected the change needs to
//! >  build the binary. It's that simple. … someone needs to indicate that this
//! >  build needs to get absorbed. … Whoever did the change does the build.
//! >  That's separate from making a compile, though. **Checking that it compiles
//! >  needs to be reserved as a no-op in terms of restarting the daemon.**"
//!
//! and, on the mechanism:
//!
//! > "instead of moving it to a new binary, I actually think you need to do the
//! >  same thing that AUI is doing by signing the binary, ideally with the same
//! >  thing that's doing the signing for AUI."
//!
//! The daemon's binary lives at `target/debug/threads` — the same path a bare
//! `cargo build` and a bare `cargo check`'s side-effect build write to. Many
//! agents share this tree. Under a plain content watch, EVERY compile check any
//! of them ran was a deploy: the watcher saw new bytes and restarted the live
//! daemon into code nobody had decided to ship.
//!
//! So the watcher no longer asks "did the bytes change". It asks **"is this
//! binary carrying [`STABLE_BUNDLE_ID`]'s designated requirement"** — i.e. did a
//! human run `lib/threads/scripts/build-threads.sh`, which builds and then signs
//! with the `harness-codesign` identity (the SAME identity and the SAME script,
//! `harness/scripts/sign-ui-server.sh`, that signs the AUI). Only the private key
//! behind that certificate can mint the signal, so `cargo build` cannot forge it
//! and never will: cargo's own output is *ad-hoc* ("linker-signed"), whose
//! designated requirement is a bare `cdhash H"…"` naming no identifier.
//!
//! Two functions, deliberately distinct — see [`binary_identity`] and
//! [`absorbable_identity`]:
//!
//! * [`binary_identity`] is TOTAL: every readable file has one. It is the code
//!   identity used for the boot reconcile, the last-good comparison and the
//!   in-window re-check.
//! * [`absorbable_identity`] is the one the WATCHER acts on, and it is `None`
//!   for anything not stably signed. An unsigned build is not "a new binary that
//!   we are choosing not to take" — it is **not a deploy at all**, and
//!   [`UpdateGate::scan_binary`] returns without touching the pending record.
//!
//! Why rejection rather than "fingerprint unsigned builds to a constant": with a
//! constant, a daemon already running a SIGNED image would see the constant
//! differ from its boot identity the moment somebody ran `cargo check`, and
//! restart. The whole requirement inverts. Not-absorbable has to be its own
//! answer, not a value in the same space.
//!
//! Note what this deliberately does NOT do, unlike `apps/aui/src/restart.rs`:
//! **this daemon never signs its own binary.** AUI signs defensively because an
//! unsigned AUI silently loses the operator's microphone grant, so AUI wants
//! every build signed however it arrived. Here the signature is not a safety
//! property (this binary holds no TCC grants) — it is the *intent* to deploy,
//! and a daemon that signed its own binary would be manufacturing that intent on
//! behalf of the person who only wanted to know whether it compiled.
//!
//! # Coming back
//!
//! **`execv` in place.** The live daemon runs detached with pid 1 as its parent
//! (AUI starts it in its own process group). `execv` replaces this process image
//! with the new binary: same pid, same parent, same argv, same environment.
//! Nothing has to notice, and — the property that matters most — **if the exec
//! fails, we are still the old daemon**, admission reopens, and the operator is
//! never left with no daemon at all.
//!
//! ## What changed under this module, 2026-08-17: there IS a supervisor now
//!
//! An earlier version of this comment said "there is no supervisor, and no
//! launchd job". **That is no longer true, and a reader must not lean on it.**
//! Both exist and are loaded:
//!
//! * `~/Library/LaunchAgents/com.harness.threads.plist` — `RunAtLoad=true`,
//!   `KeepAlive.SuccessfulExit=false`, `ThrottleInterval=30`. It supervises the
//!   SUPERVISOR, never the daemon directly.
//! * `~/.threads-supervisor/supervise-service.sh threads` — owns the spawn gate,
//!   the self-check gate and the last-good fallback, and ADOPTS a healthy daemon
//!   rather than spawning a second one.
//!
//! `execv` is still right, and for a reason that never depended on the absence
//! of a supervisor: an exec that fails leaves us running, whereas an exit is
//! unconditional and hands the outcome to a restarter that will be starting from
//! the same bad binary we just refused. Same argument, load-bearing on its own.
//!
//! Rust's std sets `O_CLOEXEC` on sockets, so the listener closes across the
//! exec and the new image rebinds port 3651; SQLite's unix VFS does the same for
//! its files, so no fds leak. The store is crash-safe by construction (WAL), and
//! the window guarantees there is nothing in flight to lose.
//!
//! AUI's client already reconnects with backoff and backfills from its per-thread
//! `after_seq` cursor, so a client with an open socket loses no work — it sees a
//! disconnect and a new `boot_id` on the next `Welcome`.
//!
//! # The last-good copy is named by a COMMIT
//!
//! [`UpdateGate::keep_last_good`] records the running image as the one known to
//! boot. Its job is to keep that copy TRACKING the daemon, and for a long while
//! it did not, because of how it decided whether it already had.
//!
//! ## The failure this section exists to explain
//!
//! Measured in the operator's live store on 2026-10-08:
//!
//! ```text
//! -rwxr-xr-x  uchg  16420144  Oct  7 16:15  threads.copying
//! -rwxr-xr-x  -     45543120  Oct  7 12:51  threads.last-good
//! ```
//!
//! The first file is a 16 MB IMMUTABLE orphan that nothing could remove. The
//! second is a debug image from four hours earlier — the safety net, frozen,
//! pointing at a build the daemon was not running. The chain:
//!
//! 1. `promote` holds the deploy path `chflags uchg` after signing it. That flag
//!    is load-bearing and must not be removed: it is the ONLY guard that survives
//!    a fully cached cargo uplift, which unlinks and recreates the product while
//!    running no build script at all. `lib/threads/build.rs` measured that.
//! 2. [`std::fs::copy`] on macOS goes through `copyfile`, whose metadata set
//!    includes `st_flags`. It carries the immutable flag across **with the
//!    bytes** — proved in scratch, with a flagless control through the same code
//!    so the measurement is known to see a difference:
//!
//!    ```text
//!    immutable source:  copy Ok, rename EPERM, remove EPERM, temp left behind
//!    flagless source:   copy Ok, rename Ok,    remove NotFound, temp consumed
//!    ```
//! 3. So the temp was born immutable, the `rename` onto `threads.last-good` got
//!    `EPERM`, and the `remove_file` on the error path got `EPERM` too. Boot one
//!    created the orphan. **Every boot after it failed earlier still, at the
//!    copy**, because the temp it wanted to write could not be opened.
//!
//! The temp is `threads.copying`, not `threads.last-good.copying`:
//! [`std::path::Path::with_extension`] REPLACES an extension and
//! `threads.last-good` has one. Worth knowing before looking for it on a disk.
//!
//! ## Why that is an identity bug and not a copy bug
//!
//! The mechanism named "the last good build" by HOLDING A SIGNED FILE and
//! comparing its cdhash. A cdhash is a name for *these bytes*: it can answer
//! whether two files are identical and nothing else. You cannot look one up,
//! order it, diff it, or ask a repository anything about it, and nothing about it
//! survives being copied — which is why a copy was load-bearing enough to break
//! the whole net when it stopped working.
//!
//! A commit is nameable, comparable, ordered, and checkable by anyone with the
//! repository. This daemon could already say which commit it is: `--build-stamp`
//! prints a commit `build.rs` compiled in, and `promote` already reads that back
//! off the product before it signs anything, precisely because "I ran a build and
//! then signed the result" is not evidence about which commit the bytes came
//! from. That is the precedent; this applies it one level down.
//!
//! * [`crate::image::build_commit`] is the running image's version identity, and
//!   is `None` unless the commit NAMES THESE BYTES. A dirty tree, or an unknown
//!   dirty flag, both collapse to `None` — and an unknown flag is not clean:
//!   `THREADS_BUILD_DIRTY` was empty for every clean build in this daemon's
//!   history, so reading empty as clean is a known-bad inference.
//! * [`commit_of`] asks a binary on disk the same question by running it. It
//!   ASKS rather than remembers, deliberately: a sidecar recording "last-good is
//!   commit X" is a second home for the truth that can disagree with the file
//!   beside it, which relocates this defect class instead of retiring it.
//! * [`last_good_verdict`] is the decision, pure and separate, because it is the
//!   part that arbitrates. **Silence decides nothing**: agreeing commits return
//!   early, differing commits promote, and a `None` on either side falls through
//!   to the byte identity, which is total and is exactly what this comparison was
//!   before a commit was the name of anything. An image that predates `build.rs`
//!   is not thereby refused.
//!
//! One consequence is substantive rather than cosmetic: a clean rebuild of the
//! same commit that produces different bytes is no longer a version change, so it
//! no longer reinstalls 16 MB to record a difference that is not one.
//!
//! ## And the mechanism, which is now small because the identity carries it
//!
//! Flags are cleared on the TEMP after the copy, never on the deploy path —
//! clearing them there would hand every cached `cargo build` in every worktree
//! the operator's running daemon. A stale temp is cleared and removed before
//! copying, flags first, which is the SELF-HEAL that was missing: the orphan is
//! exactly the state that wedged every later boot, and a daemon that cannot clear
//! its own temp now says so in one line naming the consequence rather than
//! reporting "could not copy". The failed-rename path clears flags before
//! unlinking, so the error path cannot create what it is trying to clean up.
//!
//! # Preflight and last-good
//!
//! Mirroring `recover.rs`. The code signature is checked at the WATCH, not
//! here — see "The absorb signal" above. The preflight deliberately does NOT
//! require a signature, and that is not an oversight: the last-good restore path
//! ends up exec'ing `<data_dir>/threads.last-good`, a copy of whatever image
//! last booted successfully, which may predate any of this and be unsigned. A
//! signature gate here would convert "the rescue copy is old" into "REFUSE, no
//! usable last-good" and take away the fallback in exactly the situation it
//! exists for. The signature answers *should we deploy*; the preflight answers
//! *is this thing runnable*. Do not merge them.
//!
//! Before the window we check that the candidate:
//!
//! 1. exists, is a regular file, is executable, and is at least
//!    [`MIN_BINARY_BYTES`] (catches a build interrupted mid-link),
//! 2. starts with a Mach-O or ELF magic number (catches a text file or a
//!    truncated header),
//! 3. **runs**: we exec it as `threads --self-check` and require exit 0 plus
//!    [`SELF_CHECK_OK`] on stdout. This is the "be the thing you think it is"
//!    check — it proves the candidate loads on this machine, is our program, and
//!    is not a half-written copy of something else.
//!    The candidate's FIRST exec is paid beforehand under a separate, larger
//!    bound (`THREADS_FIRST_EXEC_BUDGET_MS`, default 120 s; R9.U1), because a
//!    deploy is always a freshly written file and its first exec on macOS
//!    takes 15-36 s here. The 20 s bound then times a warm exec only.
//!
//! If the candidate fails, the last-good copy at `<data_dir>/threads.last-good`
//! is restored over it and re-verified. If there is no usable last-good either,
//! we **REFUSE to exec** and stay on the running build. That is a deliberate
//! divergence from `recover.rs`, which restarts anyway.
//!
//! The refusal was originally justified by "this daemon has nothing behind it".
//! **That justification is stale** (see the supervisor note under "Coming back"),
//! but the refusal is KEPT, because it never actually needed it: what we would be
//! refusing is an `execv`, and a failed `execv` leaves the old daemon running.
//! Refusing costs nothing except staying on code that demonstrably works, while
//! going ahead means exec'ing an image we just proved does not run — and the
//! supervisor behind us would restart into that same unrunnable binary, not a
//! better one. "Keep running the old code and say so loudly" is the honest floor
//! whether or not anything is standing behind us.
//!
//! Inside the window the approved binary is re-hashed one last time and must
//! still match; a build that landed between preflight and the window aborts the
//! window rather than exec'ing something unverified.
//!
//! # The durable pending record and the decision log
//!
//! * `<data_dir>/pending-restart.json` — the queued restart and its reason,
//!   written atomically. It survives being deferred indefinitely, and it survives
//!   the daemon being killed while deferring. At boot it is reconciled: if the
//!   image we just booted from already carries the identity the record was
//!   waiting for, the record is satisfied and cleared.
//! * `<data_dir>/restarts.jsonl` — one JSON line per decision: `queued`,
//!   `deferred` (with the blockers, deduplicated so a long defer is one line per
//!   change, not one per tick), `preflight`, `firing`, `refused`. The next person
//!   can read why it waited.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::event::Push;

/// The file name the watcher will arm itself for. A binary called anything else
/// — a `cargo test` harness, a renamed copy — is never watched and never
/// exec'd, so the mechanism cannot fire inside a test process by accident.
pub const DAEMON_EXE_NAME: &str = "threads";

/// The argv the preflight runs the candidate binary with.
pub const SELF_CHECK_FLAG: &str = "--self-check";

/// What a healthy candidate prints on stdout for [`SELF_CHECK_FLAG`].
pub const SELF_CHECK_OK: &str = "threads self-check ok";

/// The durable queued-restart record, under the data dir.
pub const PENDING_FILE: &str = "pending-restart.json";

/// The decision log, under the data dir.
pub const RESTART_LOG: &str = "restarts.jsonl";

/// The known-good image, under the data dir.
pub const LAST_GOOD: &str = "threads.last-good";

/// Below this a "binary" is a truncated write, not a build.
pub const MIN_BINARY_BYTES: u64 = 1024 * 1024;

/// How long a WARM candidate gets to answer `--self-check`.
///
/// This bound is only meaningful because [`usable`] first pays the candidate's
/// first exec under [`first_exec_budget`] (R9.U1). Timed cold, it measured the
/// machine's executable assessment rather than the binary, and refused healthy
/// deploys.
pub const SELF_CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Default for `THREADS_FIRST_EXEC_BUDGET_MS`: the bound on a candidate's FIRST
/// exec, which [`usable`] pays before the 20 s timed check.
///
/// Why it exists (R9.U1): on this machine the first exec of a newly written
/// executable pays a system-wide assessment (XProtect) that the 20 s
/// [`SELF_CHECK_TIMEOUT`] cannot absorb. MEASURED WORST CASE: 36 s (R9.F2, a
/// fresh copy of `threads --self-check` under an 8-worker exec storm; 22.5 s
/// unstormed, load 59-96; R9.C5b 4-20.5 s). The second exec of the same file
/// takes ~0.1 s. 120 s is ~3x the measured worst case. It is still a BOUND, not
/// "untimed": a candidate that hangs on every exec is given up on here and
/// refused, never waited on forever.
pub const FIRST_EXEC_BUDGET_MS: u64 = 120_000;

/// The bound on a candidate's first exec; see [`FIRST_EXEC_BUDGET_MS`].
pub fn first_exec_budget() -> Duration {
    Duration::from_millis(env_ms("THREADS_FIRST_EXEC_BUDGET_MS", FIRST_EXEC_BUDGET_MS))
}

/// How long after a refused attempt before the expensive preflight is retried.
/// A new binary identity clears it immediately.
const REFUSAL_BACKOFF: Duration = Duration::from_secs(60);

/// A defer whose blockers have not changed is re-logged this often. NOT never:
/// the original dedup-forever is why an hour of starvation left one ledger line
/// and read as "seven deferrals" instead of "hundreds".
const DEFER_RELOG: Duration = Duration::from_secs(300);

/// Default for `THREADS_RESTART_ESCALATE_SECS`. Past this a pending restart is
/// starving, not waiting. Ten minutes: with the zero-crossing hold in place a
/// deploy lands at the first turn boundary, so ten minutes of waiting means
/// something structural is wrong and a human should be able to see it.
const ESCALATE_AFTER_SECS: u64 = 600;

/// Default for `THREADS_RESTART_ESCALATE_EVERY_SECS` — how often the escalation
/// repeats once it has started. Twelve loud lines an hour, not one.
const ESCALATE_EVERY_SECS: u64 = 300;

/// Default for `THREADS_RESTART_HOLD_MS`. A window held at a zero-crossing
/// should be redeemed in tens of milliseconds; this is the self-heal bound, not
/// an expectation. A daemon may refuse turns because it is about to restart; it
/// must never refuse them forever because of a bug in the watcher.
const HOLD_LIMIT_MS: u64 = 10_000;

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn env_ms(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// How long a held window may stay held before the daemon heals itself.
pub fn hold_limit() -> Duration {
    Duration::from_millis(env_ms("THREADS_RESTART_HOLD_MS", HOLD_LIMIT_MS))
}

/// How old a pending restart has to be before it is called starvation.
fn escalate_after() -> Duration {
    Duration::from_secs(env_ms("THREADS_RESTART_ESCALATE_SECS", ESCALATE_AFTER_SECS))
}

fn escalate_every() -> Duration {
    Duration::from_secs(env_ms(
        "THREADS_RESTART_ESCALATE_EVERY_SECS",
        ESCALATE_EVERY_SECS,
    ))
}

/// How long a queued restart has been waiting, from its DURABLE timestamp — so
/// the clock keeps running across a daemon exit that happened while deferring.
pub fn pending_age(p: &PendingRestart) -> Option<Duration> {
    let queued = chrono::DateTime::parse_from_rfc3339(&p.queued_at).ok()?;
    let secs = (chrono::Utc::now() - queued.with_timezone(&chrono::Utc)).num_seconds();
    (secs >= 0).then(|| Duration::from_secs(secs as u64))
}

// ──────────────────────────────────────────────────────────────────────────
//  The lifecycle announcement  (the channel, not a threads feature)
// ──────────────────────────────────────────────────────────────────────────

/// **A SUPERVISED SERVICE SAYING OUT LOUD THAT IT IS GOING DOWN, OR THAT IT IS
/// BACK — AND WHY.**
///
/// Deliberately generic. The operator asked for this as a channel, not a
/// threads feature: *"Same with maybe text-to-speech. Any other systems that we
/// end up subscribing to, I'd like to hear that in my audio user interface."*
/// So the frame carries a `service` and nothing about this daemon is baked into
/// its shape — a second service emits the identical frame with its own id and
/// the interface needs no new code to speak it.
///
/// **Nothing here is a new vocabulary.** `reason` is [`RestartReason::kind`]'s
/// two strings (`new_binary`, `operator`) plus `fallback`, which the existing
/// [`Preflight::Restored`] decision already names, plus `unknown`, which is what
/// honesty requires when a boot cannot account for itself. `phase` is the pair
/// of edges the restart ledger already brackets a restart with — `armed`/
/// `firing` on one side, `boot` on the other.
///
/// It is TRANSIENT, in the same category as `Push::Liveness` and
/// `Push::Progress`: interesting while it is happening, meaningless afterwards,
/// and structurally incapable of being persisted. The durable record of the
/// same events is `restarts.jsonl`, which is unchanged and remains the source
/// of truth; this is the doorbell, not the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleNote {
    /// Which service. Matches the supervisor's service id (`threads`, `tts`),
    /// which is also the key the AUI's `services.json` is written against.
    pub service: String,
    /// `going_down` | `back_up`.
    pub phase: String,
    /// `new_binary` | `operator` | `fallback` | `unknown`.
    pub reason: String,
    /// One human sentence. The client switches on `reason` and never parses
    /// this — same contract as `ServerMsg::Progress::detail`.
    pub detail: String,
    /// The boot this note is about. On `going_down` it is the boot that is
    /// ending; on `back_up` it is the boot that just began, which is the same
    /// id the client sees on `Welcome` and can therefore correlate.
    pub boot_id: String,
    pub at: String,
}

/// The phases. Two, and only two: the operator asked for *"going down"* and
/// *"back up"*, and a third would be a state he did not ask to hear.
pub const PHASE_GOING_DOWN: &str = "going_down";
pub const PHASE_BACK_UP: &str = "back_up";

/// The reasons, derived (not invented) — see [`LifecycleNote`].
pub const CAUSE_NEW_BINARY: &str = "new_binary";
pub const CAUSE_OPERATOR: &str = "operator";
pub const CAUSE_FALLBACK: &str = "fallback";
pub const CAUSE_UNKNOWN: &str = "unknown";

/// **THE FOUR REASONS, AS A CLOSED VOCABULARY** — so a shape can declare
/// `reason` an `Enum` over this slice rather than over a copy of it.
///
/// It exists for the reason `crate::warnings::LEVELS` exists: a consumer that
/// paints differently for `fallback` than for `new_binary` compares against the
/// WORD, and `seamdiff` can only check that comparison against a vocabulary the
/// producer declares. Undeclared, `== "fallback"` is a predicate that may be
/// false for ever and reads, to anyone above it, like an answer.
///
/// **It is the arm set of [`UpdateGate::compute_boot_note`], and
/// `tests/cell_manifest.rs::the_boot_reason_vocabulary_is_the_derivation`
/// asserts that mechanically** — a fifth reason added to that `match` without
/// being added here would be a word on the wire that no declaration admits.
///
/// Order is the declaration order above, not a ranking: nothing reads these
/// positionally.
pub const CAUSES: &[&str] = &[
    CAUSE_NEW_BINARY,
    CAUSE_OPERATOR,
    CAUSE_FALLBACK,
    CAUSE_UNKNOWN,
];

/// This daemon's service id, as the supervisor and the AUI both spell it.
pub const SERVICE_ID: &str = "threads";

impl LifecycleNote {
    fn new(phase: &str, reason: &str, detail: impl Into<String>, boot_id: &str) -> Self {
        Self {
            service: SERVICE_ID.to_string(),
            phase: phase.to_string(),
            reason: reason.to_string(),
            detail: detail.into(),
            boot_id: boot_id.to_string(),
            at: now_rfc3339(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Reasons and the durable pending record
// ──────────────────────────────────────────────────────────────────────────

/// Why a restart is queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartReason {
    /// The on-disk image at our own path no longer has the code identity we
    /// booted with.
    NewBinary { identity: String },
    /// Somebody asked for one by hand.
    Operator { note: String },
}

impl RestartReason {
    pub fn kind(&self) -> &'static str {
        match self {
            RestartReason::NewBinary { .. } => "new_binary",
            RestartReason::Operator { .. } => "operator",
        }
    }
    pub fn describe(&self) -> String {
        match self {
            RestartReason::NewBinary { identity } => {
                format!("a new daemon binary is on disk ({identity})")
            }
            RestartReason::Operator { note } => note.clone(),
        }
    }
    fn identity(&self) -> Option<String> {
        match self {
            RestartReason::NewBinary { identity } => Some(identity.clone()),
            RestartReason::Operator { .. } => None,
        }
    }
}

/// The queued restart, as it is written to disk. Deliberately a flat struct of
/// primitives: this file has to be readable by a human with `cat` at 3am, and
/// parseable by a future daemon that does not share this enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRestart {
    pub queued_at: String,
    /// `new_binary` | `operator`
    pub kind: String,
    /// The code identity we intend to restart INTO, when the reason knows one.
    #[serde(default)]
    pub identity: Option<String>,
    pub note: String,
    pub binary: String,
}

impl PendingRestart {
    fn new(reason: &RestartReason, binary: &Path) -> Self {
        Self {
            queued_at: now_rfc3339(),
            kind: reason.kind().to_string(),
            identity: reason.identity(),
            note: reason.describe(),
            binary: binary.display().to_string(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Quiet
// ──────────────────────────────────────────────────────────────────────────

/// What is standing between us and a restart, right now.
///
/// NOTE what is NOT in here, on purpose: parked turns, waiting queue rows,
/// armed retries, and anything waiting on placement or admission. See the
/// module header — they are durable and they are the steady state, so they must
/// never delay a restart.
///
/// And note what IS in here but is NOT a blocker: [`Blockers::turns`]. It is
/// reported, never tested. See [`Blockers::quiet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blockers {
    /// Executions holding a turn lock. **REPORTED ONLY — read the doc on
    /// [`Blockers::quiet`] before using this for anything.**
    pub turns: usize,
    /// Turns inside the spawn window: after `cmd.spawn()`, before
    /// `upsert_running_turn`. **This is the blocker.**
    pub spawning: usize,
    /// WS requests being served.
    pub requests: usize,
}

impl Blockers {
    /// **`turns` IS DELIBERATELY ABSENT FROM THIS PREDICATE.**
    ///
    /// A running turn survives an `execv` — its own process group, a file for
    /// stdout, a durable `running_turns` row, and `recover()` → `adopt_turn` on
    /// the other side. Waiting for turns to reach zero is waiting for a
    /// fleet-wide simultaneous idle, which on this daemon measured 266 minutes
    /// and counting. The full argument, with its evidence, is in the module
    /// header under "THE QUIET CONDITION".
    ///
    /// The one state that cannot survive an `execv` is a child that has been
    /// spawned and not yet recorded, and that is `spawning`.
    pub fn quiet(&self) -> bool {
        self.spawning == 0 && self.requests == 0
    }
    pub fn describe(&self) -> String {
        format!(
            "{} turn(s) spawning, {} request(s) in flight ({} turn(s) executing, which do not \
             block a restart)",
            self.spawning, self.requests, self.turns
        )
    }

    /// **IDLE: [`Blockers::quiet`] AND NO TURN EXECUTING** (R9.C7, gap C-7).
    ///
    /// NOT this daemon's own restart predicate — that is [`Blockers::quiet`],
    /// and the module header says why `turns` must stay out of it. This is the
    /// stricter fact an OUTSIDE restarter reads: cauldron's re-pin restart is
    /// a SIGTERM and a relaunch, and until gap L-2 is closed an adopted turn
    /// can land wrong after one (a duplicate paid turn, or a lost message). So
    /// cauldron waits for `idle` and threads itself keeps waiting for `quiet`;
    /// which of the two cauldron reads is one line in `services/threads.toml`
    /// (`[quiet] pointer`), not code.
    pub fn idle(&self) -> bool {
        self.quiet() && self.turns == 0
    }

    /// **THE RESTART GATE, AS `/health` CARRIES IT** (R9.C7). One object under
    /// one key, so a reader addresses a field with a JSON pointer
    /// (`/restart_gate/idle`) and a field it does not know is ignored, never
    /// mistaken. The counters ride along so a refusal can say WHAT was busy.
    pub fn health_extras(&self) -> std::collections::BTreeMap<String, Value> {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "restart_gate".to_string(),
            json!({
                "quiet": self.quiet(),
                "idle": self.idle(),
                "turns": self.turns,
                "spawning": self.spawning,
                "requests": self.requests,
            }),
        );
        m
    }
}

/// What one attempt at the window did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowOutcome {
    /// The exec happened (or, under a test hook, would have). Admission is left
    /// CLOSED: in production nothing after this point runs.
    Fired,
    /// Something was running or being served. Nothing was changed.
    Busy(Blockers),
    /// We reached the quiet window and then refused to exec. Admission reopened.
    Refused(String),
}

// ──────────────────────────────────────────────────────────────────────────
//  Preflight
// ──────────────────────────────────────────────────────────────────────────

/// What the preflight decided about the binary we are about to become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preflight {
    /// The on-disk image is usable. Restart into it.
    Ok,
    /// It was not, and the last-good copy was restored over it first.
    Restored,
    /// It was not, and there is no usable last-good copy. **We do not restart.**
    NoFallback,
}

impl Preflight {
    pub fn as_str(self) -> &'static str {
        match self {
            Preflight::Ok => "ok",
            Preflight::Restored => "restored-last-good",
            Preflight::NoFallback => "no-last-good",
        }
    }
}

/// The pure policy, so it is testable with no binary and no subprocess.
///
/// The one line that differs from `apps/aui/src/recover.rs`: `NoFallback` here
/// means REFUSE.
///
/// **The stated reason for that used to be "because this daemon has no
/// supervisor to catch it". That reason expired on 2026-08-17** — there is now a
/// launchd job (`com.harness.threads`) driving `~/.threads-supervisor/
/// supervise-service.sh threads`, with `RunAtLoad` and `KeepAlive`. See the
/// module header.
///
/// The refusal STAYS, on its own merits, and a later reader should not delete it
/// because the old justification went away:
///
/// * What we refuse is an `execv`. A refused `execv` leaves this process running
///   the code it is already running. An `execv` that *fires* into a binary we
///   just failed to verify is the only way to end up with no daemon at all.
/// * `NoFallback` means the candidate did not run AND the last-good copy did not
///   run either. Exiting into the supervisor would hand it the same two broken
///   images to choose from; it would relaunch into the same failure, throttled to
///   one attempt per 30s, forever.
/// * Refusing is loud (`refused` in `restarts.jsonl`, ERROR in the log) and
///   leaves a working daemon serving. That is strictly better than any outcome
///   available on the other branch.
pub fn decide(on_disk_ok: bool, last_good_ok: bool) -> Preflight {
    match (on_disk_ok, last_good_ok) {
        (true, _) => Preflight::Ok,
        (false, true) => Preflight::Restored,
        (false, false) => Preflight::NoFallback,
    }
}

/// The stable bundle identifier `harness/scripts/sign-ui-server.sh` signs this
/// daemon with — the SAME script and the SAME `harness-codesign` certificate the
/// AUI uses, invoked by `lib/threads/scripts/build-threads.sh` with this id.
///
/// Kept in sync with that script's second argument by hand. Both are small and
/// rarely touched, and this constant exists specifically so the Rust side does
/// NOT have to trust the script — see [`is_stably_signed`]. The AUI's twin is
/// `apps/aui/src/restart.rs`'s `STABLE_BUNDLE_ID`.
pub const STABLE_BUNDLE_ID: &str = "com.harness.threads";

/// SHA-256 of a file, streamed — the debug binary is 27 MB, don't slurp it.
fn file_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("sha256:{hex}"))
}

/// Does `path`'s DESIGNATED REQUIREMENT actually name [`STABLE_BUNDLE_ID`]?
///
/// This is the whole absorb signal — "a human ran the build script" — and it is
/// asked of the file itself, never of whatever ran the signing. Mirrors
/// `apps/aui/src/restart.rs::is_stably_signed` exactly, for a different reason:
/// there it gates the microphone grant, here it gates the deploy.
///
/// The DR is the right thing to read rather than `Identifier=` from `-dvvv`,
/// because the DR our signer produces is
///
/// ```text
/// identifier "com.harness.threads" and certificate leaf = H"9a05af73…"
/// ```
///
/// — it pins the CERTIFICATE, so only somebody holding the `harness-codesign`
/// private key can mint the signal. `codesign --sign -` (ad-hoc), which is what
/// cargo's linker leaves behind, produces `cdhash H"…"` and names no identifier
/// at all, so a bare `cargo build` can never accidentally look like a deploy no
/// matter what identifier flags anyone passes it.
pub fn is_stably_signed(path: &Path) -> bool {
    std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r", "-"])
        .arg(path)
        .output()
        .map(|o| {
            // codesign writes this to stdout on success and stderr on failure;
            // read both rather than guess.
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            text.contains(STABLE_BUNDLE_ID)
        })
        .unwrap_or(false)
}

/// The binary's *code* identity. TOTAL: any readable file has one.
///
/// Prefers the code-directory hash from `codesign -dvvv`, exactly as
/// `apps/aui/src/restart.rs::binary_identity` does, and for the reason that cost
/// the AUI a restart loop:
///
/// > the CDHash covers the executable pages and the signing identifier but NOT
/// > the CMS signature blob, so it changes on every real rebuild yet stays
/// > identical across re-signs — which embed a fresh signing timestamp and
/// > therefore change the whole-file hash. Hashing the raw file made the watcher
/// > treat its own signing pass as a new binary.
///
/// Measured here on 2026-08-17 rather than assumed: signing the same image twice
/// gives `CDHash=d7f68c19…` both times and two DIFFERENT SHA-256s. Threads would
/// have walked into the same trap the moment signing became the deploy step —
/// build, sign, watcher sees new bytes, restarts, and the next sign starts it
/// again.
///
/// Falls back to a whole-file SHA-256 when `codesign` reports no CDHash (a
/// non-Mach-O fixture, a truncated file, a Linux build).
///
/// **Known property, deliberately accepted.** `codesign -dvvv` REPORTS the
/// stored code directory; it does not recompute page hashes. So editing bytes in
/// an already-signed Mach-O leaves the reported CDHash unchanged. That is fine
/// for every caller here, because no real producer edits a Mach-O in place — the
/// linker and `codesign` both rewrite the file and regenerate the code directory,
/// which is precisely the event we want to notice. It does mean this is a *code*
/// identity, not a tamper check, and nothing in this module should be rewritten
/// to lean on it as one.
pub fn binary_identity(path: &Path) -> Option<String> {
    if let Ok(o) = std::process::Command::new("/usr/bin/codesign")
        .arg("-dvvv")
        .arg(path)
        .output()
    {
        // codesign writes its info output to stderr.
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stderr),
            String::from_utf8_lossy(&o.stdout)
        );
        if let Some(line) = text.lines().find(|l| l.starts_with("CDHash=")) {
            return Some(format!("cdhash:{}", line.trim_start_matches("CDHash=").trim()));
        }
    }
    file_sha256(path)
}

/// The identity the WATCHER is allowed to act on — `None` unless the image is
/// stably signed.
///
/// `None` here means **"not a deploy"**, not "unreadable" and not "a new binary
/// we are declining". [`UpdateGate::scan_binary`] treats it as nothing happened:
/// it does not queue, does not clear a pending record, does not log.
///
/// This is the function that makes `cargo check` a no-op, and the reason it is
/// separate from [`binary_identity`] rather than being a mode of it:
///
/// * [`binary_identity`] must stay TOTAL. It is what `UpdateGate::new` records
///   as `boot_identity`, what `keep_last_good` compares, and what `fire`
///   re-checks inside the window. If it went `None` on an unsigned image, a
///   daemon that happened to boot from an unsigned build would disarm its own
///   update mechanism entirely and could never absorb the first signed build.
/// * The alternative — fingerprinting every unsigned image to one CONSTANT so
///   successive `cargo build`s compare equal — inverts the requirement in the
///   case that matters most. A daemon running a SIGNED image has a `cdhash:…`
///   boot identity; the first `cargo check` anyone ran would make the on-disk
///   identity that constant, which differs, and the daemon would restart into an
///   unsigned build. Rejection has to be its own answer.
pub fn absorbable_identity(path: &Path) -> Option<String> {
    if !is_stably_signed(path) {
        return None;
    }
    binary_identity(path)
}

/// Are these two paths the same file on disk?
///
/// `(dev, ino)`, not string equality: the supervisor execs the last-good copy by
/// an absolute path built from `$THREADS_DATA_HOME`, and `current_exe()` returns
/// the kernel's own answer — the two agree on the file and need not agree
/// character-for-character. The pair is compared because an inode is unique only
/// within a filesystem, which is the same reasoning `image::divergence` uses.
///
/// `false` when either side cannot be stat'd. A missing last-good copy is not a
/// fallback, and an unanswerable question is never answered `true`.
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// Present, a regular file, executable, and not a truncated write.
pub fn plausible(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(m) = std::fs::metadata(path) else {
        return false;
    };
    if !m.is_file() || m.len() < MIN_BINARY_BYTES {
        return false;
    }
    if m.permissions().mode() & 0o111 == 0 {
        return false;
    }
    magic_ok(path)
}

/// Mach-O (thin or universal) or ELF. Catches "somebody left a shell script /
/// a text file / half a header here".
fn magic_ok(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 4];
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    matches!(
        u32::from_be_bytes(head),
        // Mach-O 64 LE/BE, Mach-O 32 LE/BE, universal (fat) LE/BE
        0xcffa_edfe | 0xfeed_facf | 0xcefa_edfe | 0xfeed_face | 0xcafe_babe | 0xbebafeca
    ) || &head == b"\x7fELF"
}

/// Run the candidate as `<path> --self-check` and require a clean answer.
///
/// This is the strongest honest statement available about a binary we are about
/// to become: it loads, it is our program, and it agrees about its own version.
pub async fn self_check(path: &Path) -> std::result::Result<String, String> {
    let out = tokio::time::timeout(
        SELF_CHECK_TIMEOUT,
        tokio::process::Command::new(path)
            .arg(SELF_CHECK_FLAG)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("{} did not answer --self-check in time", path.display()))?
    .map_err(|e| format!("could not run {} --self-check: {e}", path.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        return Err(format!(
            "{} --self-check exited {:?}",
            path.display(),
            out.status.code()
        ));
    }
    if !stdout.contains(SELF_CHECK_OK) {
        return Err(format!(
            "{} --self-check did not identify itself as this daemon",
            path.display()
        ));
    }
    Ok(stdout.trim().to_string())
}

/// Pay the candidate's FIRST exec, bounded by [`first_exec_budget`] and not by
/// the 20 s [`SELF_CHECK_TIMEOUT`] (R9.U1).
///
/// A deploy, a last-good copy and a restored copy are all FRESHLY WRITTEN files,
/// and on macOS the first exec of a fresh file is assessed system-wide — 15-36 s
/// measured here, per FILE, not per content. So this runs `--self-check` once,
/// output discarded, and only reports whether the candidate FINISHED within the
/// budget. Its exit status and output are judged by the timed [`self_check`]
/// that follows, which stays the single authority on "is this our daemon".
async fn warm_first_exec(path: &Path) -> std::result::Result<(), String> {
    let budget = first_exec_budget();
    let started = Instant::now();
    let ran = tokio::time::timeout(
        budget,
        tokio::process::Command::new(path)
            .arg(SELF_CHECK_FLAG)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await;
    match ran {
        Err(_) => Err(format!(
            "{} did not finish its first --self-check within the first-exec budget of {budget:?} \
             (THREADS_FIRST_EXEC_BUDGET_MS)",
            path.display()
        )),
        Ok(Err(e)) => Err(format!("could not run {} --self-check: {e}", path.display())),
        Ok(Ok(_)) => {
            tracing::info!(
                binary = %path.display(),
                first_exec_ms = started.elapsed().as_millis() as u64,
                "update: candidate first exec paid before the timed self-check"
            );
            Ok(())
        }
    }
}

/// Usable = plausible on disk AND it actually runs and says who it is.
///
/// The first exec is paid under [`first_exec_budget`]; THEN the 20 s
/// [`self_check`] decides. A hang past the budget, a non-zero exit or a wrong
/// banner is still unusable.
pub async fn usable(path: &Path) -> bool {
    if !plausible(path) {
        return false;
    }
    if let Err(why) = warm_first_exec(path).await {
        tracing::warn!("update: {why}");
        return false;
    }
    self_check(path).await.is_ok()
}

/// What `prepare` approved.
#[derive(Debug, Clone)]
pub struct Ready {
    pub binary: PathBuf,
    pub identity: String,
    pub preflight: Preflight,
    pub banner: String,
}

// ──────────────────────────────────────────────────────────────────────────
//  The gate
// ──────────────────────────────────────────────────────────────────────────

struct GateInner {
    /// May a NEW turn execution start? Closed only for a restart window.
    admitting: bool,
    /// Set when admission was closed by [`UpdateGate::consider_hold`] at a
    /// zero-crossing and the exec has not been reached yet. `None` once the
    /// window is actually firing (that closure is permanent — the process is
    /// about to be gone), so the self-heal below can tell the two apart and
    /// never reopens a daemon that is mid-`execv`.
    held_at: Option<Instant>,
    /// Outstanding [`TurnPermit`]s. Reported on every ledger line; NOT part of
    /// [`Blockers::quiet`].
    turns: usize,
    /// Outstanding [`SpawnPermit`]s — turns between `cmd.spawn()` and
    /// `upsert_running_turn`. THIS is the counter the quiet condition reads.
    spawning: usize,
    requests: usize,
    /// **A DRAIN, AND WHEN IT BEGAN** (R9.C7c, gap C-7). `Some` while an
    /// outside restarter (cauldron, re-pinning this service) has asked this
    /// process to stop STARTING turns so the ones running can finish before it
    /// is replaced. Independent of `admitting`, which belongs to this daemon's
    /// own restart window and is reopened by that window's own logic; a drain
    /// is ended only by [`UpdateGate::end_drain`] or by the process exiting.
    /// Never persisted: the process that comes back after the restart starts
    /// open, and it is the one that runs the queue.
    draining_since: Option<SystemTime>,
    /// **WHICH threads hold a [`TurnPermit`]** (R9.C7b, E18-15), with a count
    /// per id. Reporting only, like `turns`: `/health` serves the ids as
    /// `restart_gate.turn_threads`, so `cauldron status` can name the turns a
    /// drain is waiting for. Never read to decide anything.
    turn_threads: std::collections::BTreeMap<String, usize>,
}

/// The stand-in a test puts where the `execv` goes. `Ok(())` means "from here
/// the process would be gone"; `Err` means the exec failed and we are still the
/// old daemon.
pub type ExecHook = Box<dyn Fn(&Path) -> std::result::Result<(), String> + Send + Sync>;

/// The process-wide update gate. One per [`crate::ThreadManager`], shared by
/// every clone of it.
pub struct UpdateGate {
    inner: StdMutex<GateInner>,
    pending: StdMutex<Option<PendingRestart>>,
    data_dir: PathBuf,
    /// Our own executable, resolved once at construction.
    exe: Option<PathBuf>,
    /// The identity of the image we booted from.
    boot_identity: Option<String>,
    /// Woken whenever a permit is released or a restart is queued, so the
    /// window is attempted the INSTANT the last turn ends rather than at the
    /// next tick (the AUI's "poke", same reasoning).
    wake: tokio::sync::Notify,
    /// **The pre-approved candidate.** Filled by [`UpdateGate::arm`] while the
    /// daemon is BUSY, so that the only thing left to do at the zero-crossing is
    /// one identity re-check and the `execv`. This is the starvation fix: the
    /// expensive preflight no longer sits between the quiet check and the act.
    armed: StdMutex<Option<Ready>>,
    /// `armed.is_some()`, readable without taking `armed` — so
    /// [`UpdateGate::consider_hold`] can consult it while it already holds
    /// `inner`, and there is no second lock to order against the first.
    armed_flag: AtomicBool,
    /// The last deferral we logged and when, so a long defer is a handful of
    /// lines — but never, as it once was, exactly one line and then silence.
    last_defer: StdMutex<Option<(String, Instant)>>,
    /// When the starvation escalation last shouted.
    escalated_at: StdMutex<Option<Instant>>,
    /// Don't re-run the (subprocess-spawning) preflight every tick after a
    /// refusal.
    retry_after: StdMutex<Option<Instant>>,
    /// The last (len, mtime) we saw, so the common case is one `stat`.
    last_stat: StdMutex<Option<(u64, SystemTime)>>,
    /// Test seam: replaces the `execv`. `Ok(())` means "treat it as gone".
    exec_hook: StdMutex<Option<ExecHook>>,
    watcher_started: AtomicBool,
    /// **THE ANNOUNCEMENT CHANNEL.** The firehose, handed down from
    /// [`crate::runtime::ThreadManager`] so the gate can say out loud what it is
    /// about to do. `None` in every non-daemon construction (tests, CLI verbs),
    /// which is why every emit site goes through [`UpdateGate::announce`] and
    /// silently does nothing rather than unwrapping.
    /// **THE SIXTH EMIT SITE.** It used to be a CLONED `broadcast::Sender<Push>`
    /// (wired at `runtime.rs:846` and `:863`), which meant this one announcer
    /// bypassed `Hub::emit` entirely — no stamp, no FIFO — and made "the hub
    /// replaces the five emit sites" false by one. The critique's F6 found it.
    /// It is the `Hub` itself now, so there is genuinely one emit point and
    /// `Push::Lifecycle` reaches a shape's fold like every other push.
    announce: StdMutex<Option<(std::sync::Arc<crate::shape::Hub>, String)>>,
    /// **WHAT THIS BOOT HAS TO SAY FOR ITSELF**, computed once in
    /// [`UpdateGate::on_boot`] and HELD for the `boot` shape, which exports it
    /// to every subscriber that arrives afterwards (it was replayed to firehose
    /// subscribers until R2.S4 retired the firehose).
    ///
    /// It has to be held rather than merely broadcast, because of the one
    /// structural fact this whole feature is shaped around: `on_boot` runs
    /// BEFORE the server serves (see `lib.rs`), so at the instant the daemon knows why
    /// it came up there is not one socket open to tell. A client that
    /// reconnects three seconds later would otherwise learn only that the
    /// `boot_id` changed, and would have to guess the reason.
    boot_note: StdMutex<Option<LifecycleNote>>,
    /// **BOOT RECOVERY'S PROGRESS** (R9.C7g). Here because `/health` is built
    /// from this gate's [`UpdateGate::health_extras`] on both its 200 and its
    /// 503 paths, and because `run_daemon` holds a request permit on this gate
    /// for as long as recovery runs, so the quiet condition already counts it.
    recovery: crate::recovery::Recovery,
}

impl std::fmt::Debug for UpdateGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let b = self.peek();
        f.debug_struct("UpdateGate")
            .field("data_dir", &self.data_dir)
            .field("exe", &self.exe)
            .field("boot_identity", &self.boot_identity)
            .field("turns", &b.turns)
            .field("spawning", &b.spawning)
            .field("requests", &b.requests)
            .field("admitting", &self.admitting())
            .finish()
    }
}

impl UpdateGate {
    /// The gate for THIS process.
    ///
    /// The boot identity is computed only when our own executable is actually
    /// the daemon binary. Everything else — a `cargo test` harness, a CLI verb —
    /// gets `None`, which disarms the whole mechanism: no hash of a 50 MB test
    /// binary on every `ThreadManager::new`, no last-good promotion of something
    /// that is not the daemon, and no possibility of exec'ing a test process.
    pub fn new(data_dir: PathBuf) -> Arc<Self> {
        let exe = std::env::current_exe().ok();
        let is_daemon = exe
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n == DAEMON_EXE_NAME)
            .unwrap_or(false);
        let boot_identity = if is_daemon {
            exe.as_deref().and_then(binary_identity)
        } else {
            None
        };
        Self::build(data_dir, exe, boot_identity)
    }

    /// A gate pinned to an EXPLICIT binary — what the tests construct, so a test
    /// drives the real mechanism against a fixture and never against its own
    /// harness binary.
    pub fn for_binary(data_dir: PathBuf, exe: PathBuf) -> Arc<Self> {
        let boot_identity = binary_identity(&exe);
        Self::build(data_dir, Some(exe), boot_identity)
    }

    fn build(data_dir: PathBuf, exe: Option<PathBuf>, boot_identity: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            inner: StdMutex::new(GateInner {
                admitting: true,
                held_at: None,
                turns: 0,
                spawning: 0,
                requests: 0,
                draining_since: None,
                turn_threads: std::collections::BTreeMap::new(),
            }),
            pending: StdMutex::new(None),
            data_dir,
            exe,
            boot_identity,
            wake: tokio::sync::Notify::new(),
            armed: StdMutex::new(None),
            armed_flag: AtomicBool::new(false),
            escalated_at: StdMutex::new(None),
            last_defer: StdMutex::new(None),
            retry_after: StdMutex::new(None),
            last_stat: StdMutex::new(None),
            exec_hook: StdMutex::new(None),
            watcher_started: AtomicBool::new(false),
            announce: StdMutex::new(None),
            boot_note: StdMutex::new(None),
            recovery: crate::recovery::Recovery::new(),
        })
    }

    /// Boot recovery's progress (R9.C7g) — see [`crate::recovery`].
    pub fn recovery(&self) -> &crate::recovery::Recovery {
        &self.recovery
    }

    // ── the lifecycle announcement ───────────────────────────────────────

    /// Hand the gate the firehose and this process's `boot_id`.
    ///
    /// Called once by [`crate::runtime::ThreadManager::new`]. Separate from
    /// construction because the gate is built for a data directory and the
    /// firehose belongs to the manager; a gate built by a test or a CLI verb
    /// has neither and must still work.
    pub fn wire_announcer(
        &self,
        hub: std::sync::Arc<crate::shape::Hub>,
        boot_id: impl Into<String>,
    ) {
        *self.announce.lock().unwrap_or_else(|e| e.into_inner()) = Some((hub, boot_id.into()));
    }

    /// Broadcast one note. Best-effort by construction: an unwired gate, or a
    /// firehose with no subscribers, is not an error — the durable record of
    /// the same event is the line already in `restarts.jsonl`.
    fn announce(&self, phase: &str, reason: &str, detail: impl Into<String>) {
        let g = self.announce.lock().unwrap_or_else(|e| e.into_inner());
        let Some((hub, boot_id)) = g.as_ref() else {
            return;
        };
        let note = LifecycleNote::new(phase, reason, detail, boot_id);
        tracing::info!(
            service = %note.service, phase = %note.phase, reason = %note.reason,
            "lifecycle: announcing to subscribers"
        );
        hub.emit(Push::Lifecycle(note));
    }

    /// Why we are about to go down, in the vocabulary the ledger already uses.
    ///
    /// **The `fallback` branch is the honest half of the operator's *"if it is
    /// possible to do the latter"*.** There are two different fallbacks in this
    /// system and only one of them can be announced on the way down:
    ///
    /// * **This one — a PLANNED fallback.** [`Preflight::Restored`] means the
    ///   new build failed `usable()`, the last-good copy was restored over the
    ///   launch path, and the daemon is about to `execv` into the OLD image. The
    ///   daemon is alive, serving, and knows exactly what it is doing. It can
    ///   say so, and it does.
    /// * **The other one — an UNPLANNED fallback**, where the daemon crashed on
    ///   boot and the supervisor gave up on the new binary. That one is
    ///   structurally unannounceable from here and no code in this file
    ///   pretends otherwise: the process that would have to speak is the one
    ///   that died, and at the moment the supervisor decides there is no daemon
    ///   listening on the socket to broadcast through. It is reported on the way
    ///   BACK UP instead — see [`UpdateGate::compute_boot_note`].
    pub fn going_down_reason(&self, preflight: Preflight) -> (&'static str, String) {
        if preflight == Preflight::Restored {
            return (
                CAUSE_FALLBACK,
                "the staged build did not pass preflight; restarting onto the previous binary"
                    .to_string(),
            );
        }
        match self.pending().as_ref().map(|p| p.kind.clone()) {
            Some(k) if k == CAUSE_OPERATOR => (
                CAUSE_OPERATOR,
                "restarting because it was asked for".to_string(),
            ),
            _ => (
                CAUSE_NEW_BINARY,
                "restarting to load a new binary".to_string(),
            ),
        }
    }

    /// **WHY THIS BOOT HAPPENED, ANSWERED FROM FACTS THIS PROCESS CAN CHECK
    /// ABOUT ITSELF** — never from a guess, and never from a story the previous
    /// process left behind that we cannot corroborate.
    ///
    /// Three tests, in order, each of which is a measurement:
    ///
    /// 1. **Are we running the last-good file itself?** The supervisor's
    ///    fallback path sets `ACTIVE_BINARY="$LAST_GOOD"` and execs
    ///    `<data_dir>/threads.last-good` directly, so `current_exe()` IS that
    ///    path. This is the unplanned fallback, reported on the way up because
    ///    it could not be reported on the way down. It is the strongest signal
    ///    available and it is checked first.
    /// 2. **Did the pending record want the identity we actually booted with?**
    ///    If so this boot satisfied a queued restart, and that record's own
    ///    `kind` — `new_binary` or `operator` — is the reason, unchanged.
    /// 3. **Did a pending record go UNSATISFIED, and do we match the last-good
    ///    image?** Then the queued build is not what came up and the image that
    ///    did is the known-good one: a fallback, corroborated by identity
    ///    rather than assumed from the disappointment.
    ///
    /// Anything else is `unknown`, which is a real answer and the one this
    /// daemon owes when it cannot account for itself — a cold start, a `kill
    /// -9`, a crash the supervisor restarted from the same binary. The client
    /// says "it restarted" and does not invent a because.
    fn compute_boot_note(&self, pending: Option<&PendingRestart>, satisfied: bool) -> LifecycleNote {
        let boot_id = self
            .announce
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(_, b)| b.clone())
            .unwrap_or_default();

        let good = self.last_good_path();
        let running_last_good_file = self
            .exe
            .as_deref()
            .map(|e| same_file(e, &good))
            .unwrap_or(false);

        let (reason, detail) = if running_last_good_file {
            (
                CAUSE_FALLBACK,
                "came up on the previous binary — the supervisor fell back to the last-good image",
            )
        } else if satisfied {
            match pending.map(|p| p.kind.as_str()) {
                Some("operator") => (CAUSE_OPERATOR, "came back from a requested restart"),
                Some("new_binary") => (CAUSE_NEW_BINARY, "came back on a new binary"),
                _ => (CAUSE_UNKNOWN, "came back"),
            }
        } else if pending.is_some()
            && self.boot_identity.is_some()
            && binary_identity(&good) == self.boot_identity
        {
            (
                CAUSE_FALLBACK,
                "the queued build is not what came up; this image is the last-good one",
            )
        } else {
            (CAUSE_UNKNOWN, "came back")
        };

        LifecycleNote::new(PHASE_BACK_UP, reason, detail, &boot_id)
    }

    /// What this boot has to say for itself. Read by the `boot` shape
    /// (`shapes::boot`), which is its one carrier to a consumer.
    pub fn boot_note(&self) -> Option<LifecycleNote> {
        self.boot_note
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
    pub fn exe(&self) -> Option<&Path> {
        self.exe.as_deref()
    }
    pub fn boot_identity(&self) -> Option<&str> {
        self.boot_identity.as_deref()
    }
    pub fn last_good_path(&self) -> PathBuf {
        self.data_dir.join(LAST_GOOD)
    }
    pub fn pending_path(&self) -> PathBuf {
        self.data_dir.join(PENDING_FILE)
    }
    pub fn log_path(&self) -> PathBuf {
        self.data_dir.join(RESTART_LOG)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Is a new turn allowed to start? Informational — see the module header:
    /// this is NOT what makes the window safe.
    pub fn admitting(&self) -> bool {
        let g = self.lock();
        g.admitting && g.draining_since.is_none()
    }

    // ── the drain (R9.C7c, gap C-7) ──────────────────────────────────────

    /// **BEGIN A DRAIN: START NO NEW TURN; LET THE RUNNING ONES FINISH.**
    ///
    /// Asked for by the supervisor that is about to replace this process (a
    /// `cauldron pin` to a different build, while turns execute). From here
    /// [`UpdateGate::admit_turn`] refuses, so `lock_turn` answers
    /// `RestartPending` and every new prompt STAYS QUEUED exactly as it does
    /// inside this daemon's own restart window: a `send` is written to the
    /// ledger and the `queue` table before dispatch is ever attempted, a
    /// refused dispatch leaves the row `waiting`, and the process that comes
    /// back runs it from `recover()` → `resume_sweep()`. Nothing is rejected
    /// and nothing new is stored. Executions that already hold a permit —
    /// running turns, including managed children — are untouched and run to
    /// their own end.
    ///
    /// Idempotent: a second request keeps the FIRST instant, because the cap
    /// the supervisor applies is measured from it. Returns whether this call
    /// began the drain.
    pub fn begin_drain(&self) -> bool {
        let began = {
            let mut g = self.lock();
            if g.draining_since.is_some() {
                false
            } else {
                g.draining_since = Some(SystemTime::now());
                true
            }
        };
        if began {
            let b = self.peek();
            tracing::warn!(
                turns = b.turns,
                "DRAINING at the supervisor's request: no new turn starts; new prompts are \
                 queued; the {} running turn(s) finish, then this process is replaced",
                b.turns
            );
            self.log_event("drain_begun", json!({ "turns": b.turns }));
        }
        began
    }

    /// **END A DRAIN** — the pin went back to the build that is running, so
    /// nothing is going to replace this process. Admission reopens (unless this
    /// daemon's own restart window holds it) and the caller re-asks the queue.
    /// Returns whether a drain was in progress.
    pub fn end_drain(&self) -> bool {
        let ended = self.lock().draining_since.take();
        if let Some(since) = ended {
            let secs = since.elapsed().map(|d| d.as_secs()).unwrap_or(0);
            tracing::warn!(drained_secs = secs, "DRAIN CANCELLED: new turns start again");
            self.log_event("drain_cancelled", json!({ "drained_secs": secs }));
            self.wake.notify_waiters();
        }
        ended.is_some()
    }

    /// When the drain in progress began, if one is.
    pub fn draining_since(&self) -> Option<SystemTime> {
        self.lock().draining_since
    }

    /// The restart gate as `/health` carries it: [`Blockers::health_extras`]
    /// plus the drain. `draining_since` is whole seconds since the epoch, or
    /// `null` — a reader tells a drain-aware build from an older one by the
    /// KEY being present at all, which is what makes asking an older build to
    /// drain impossible (see `services/threads.toml` `[quiet.drain]`).
    pub fn health_extras(&self) -> std::collections::BTreeMap<String, Value> {
        let since = self.draining_since();
        let mut m = self.peek().health_extras();
        if let Some(Value::Object(gate)) = m.get_mut("restart_gate") {
            gate.insert("draining".into(), json!(since.is_some()));
            gate.insert("turn_threads".into(), json!(self.turn_threads()));
            gate.insert(
                "draining_since".into(),
                match since.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()) {
                    Some(d) => json!(d.as_secs()),
                    None => Value::Null,
                },
            );
        }
        // R9.C7g: what boot recovery still has to do, so a supervisor reading
        // a 200 can tell "serving" from "serving and still resuming children".
        m.insert("recovery".into(), self.recovery.health_json());
        m
    }

    /// The thread ids holding a turn permit, sorted, one entry per permit
    /// (R9.C7b). Informational.
    pub fn turn_threads(&self) -> Vec<String> {
        self.lock()
            .turn_threads
            .iter()
            .flat_map(|(id, n)| std::iter::repeat_n(id.clone(), *n))
            .collect()
    }

    /// A snapshot of the blockers. Informational.
    pub fn peek(&self) -> Blockers {
        let g = self.lock();
        Blockers {
            turns: g.turns,
            spawning: g.spawning,
            requests: g.requests,
        }
    }

    // ── permits ──────────────────────────────────────────────────────────

    /// Take a permit to START A TURN, or `None` because a restart window is
    /// open. **This is the admission gate, and it stays exactly that.**
    /// `lock_turn` calls it first and holds the permit for the whole execution.
    ///
    /// Its argument is untouched by the spawn-permit change and must not be
    /// weakened: `lock_turn` is the single door every execution in this daemon
    /// enters through, so it is the only place where "nothing is being started"
    /// can be made TRUE rather than merely likely. What changed is the SCOPE of
    /// what the quiet predicate reads, not what admission means. The permit this
    /// returns is now a REPORTING counter ([`Blockers::turns`]) plus this
    /// refusal; the blocker is [`UpdateGate::enter_spawn`].
    ///
    /// This is also where a HELD window heals. A window held at a zero-crossing
    /// should be redeemed within milliseconds; if one is still held after
    /// [`hold_limit`], the watcher that was supposed to `execv` did not, and the
    /// honest thing is to reopen and shout rather than to keep refusing turns
    /// for a restart that is not coming.
    pub fn admit_turn(self: &Arc<Self>) -> Option<TurnPermit> {
        self.admit(None)
    }

    /// [`UpdateGate::admit_turn`] for a named thread, so `/health` can say
    /// which turns are running (`restart_gate.turn_threads`, R9.C7b). The one
    /// call `lock_turn` makes.
    pub fn admit_turn_for(self: &Arc<Self>, thread: &str) -> Option<TurnPermit> {
        self.admit(Some(thread.to_string()))
    }

    fn admit(self: &Arc<Self>, thread: Option<String>) -> Option<TurnPermit> {
        let mut healed = false;
        {
            let mut g = self.lock();
            // A DRAIN REFUSES BEFORE ANYTHING ELSE, and is never healed here:
            // the hold-limit heal below is for THIS daemon's own window, which
            // is supposed to be redeemed in milliseconds. A drain is supposed to
            // last as long as the running turns do (R9.C7c).
            if g.draining_since.is_some() {
                return None;
            }
            if !g.admitting {
                let stale = g.held_at.map(|t| t.elapsed() >= hold_limit()).unwrap_or(false);
                if !stale {
                    return None;
                }
                g.admitting = true;
                g.held_at = None;
                healed = true;
            }
            g.turns += 1;
            if let Some(t) = &thread {
                *g.turn_threads.entry(t.clone()).or_default() += 1;
            }
        }
        if healed {
            tracing::error!(
                "update: a restart window was held open at a quiet moment and the restart never \
                 happened — REOPENING admission. The daemon is still running the old binary."
            );
            self.log_event("hold_expired", json!({ "limit_ms": hold_limit().as_millis() as u64 }));
        }
        Some(TurnPermit {
            gate: Some(self.clone()),
            thread,
        })
    }

    /// Take a permit for a WS request being served. Never refused — refusing a
    /// client's read because we would like to restart would be worse than
    /// waiting one request out, and the window's quiet check counts it anyway.
    ///
    /// A request that arrives during a HELD window therefore breaks it; the
    /// window reopens (see [`UpdateGate::with_quiet_window`]) and re-holds at
    /// the next zero-crossing. That is the deliberate trade: a client is never
    /// made to wait on a restart.
    pub fn enter_request(self: &Arc<Self>) -> RequestPermit {
        self.lock().requests += 1;
        RequestPermit { gate: self.clone() }
    }

    /// **THE ONE STATE THAT CANNOT SURVIVE AN `execv`.** Taken by `run_once`
    /// immediately before `cmd.spawn()` and dropped immediately after
    /// `upsert_running_turn` has written the child's identity.
    ///
    /// Inside that window a `claude` process may exist with nothing on disk
    /// naming it: `recover()` reads `running_turns`, so a child that is not
    /// there yet can neither be re-adopted nor buried, and an `execv` in the gap
    /// leaves it running forever with no parent that knows about it. Everything
    /// else about a turn is recoverable by construction (own process group,
    /// file-backed stdout, durable row, `adopt_turn` from `byte_offset`), which
    /// is why this window — and only this window — is the quiet condition.
    ///
    /// **Never refused**, for the same reason `enter_request` is never refused:
    /// the caller has already passed admission, is holding a `TurnLock`, and has
    /// nowhere to put a refusal. A spawn that starts inside a HELD window
    /// therefore breaks the hold, and `with_quiet_window` reopens the daemon and
    /// re-holds at the next zero-crossing — the identical, already-proven path a
    /// late WS request takes. The window it can break is tens of milliseconds
    /// wide, and the alternative (refusing here) would mean unwinding a turn
    /// that has already consumed its queue rows.
    pub fn enter_spawn(self: &Arc<Self>) -> SpawnPermit {
        self.lock().spawning += 1;
        SpawnPermit {
            gate: Some(self.clone()),
        }
    }

    /// **THE ZERO-CROSSING (D-28).** Called with `inner` ALREADY HELD, by the
    /// same critical section that just decremented a counter.
    ///
    /// If that decrement is the one that made the daemon quiet, and a candidate
    /// is staged, admission closes *here* — not in a watcher that has to be
    /// scheduled first. `runtime::finish_turn` releases its permit and then,
    /// with no `.await` in between, calls `maybe_dispatch` → `lock_turn` →
    /// `admit_turn`, which needs this very mutex. So the next turn does not
    /// merely lose a race to the restart: it blocks on the lock we are holding
    /// and is then refused, and its work stays queued. **The quiet instant is
    /// held open rather than chased.**
    ///
    /// Returns whether it closed the window, so the caller can log and poke the
    /// watcher after dropping the guard — no file I/O under this mutex.
    /// The predicate below is [`Blockers::quiet`], spelled out against the
    /// guard we already hold rather than through a snapshot. `turns` is
    /// deliberately not one of its terms — see [`Blockers::quiet`]. In practice
    /// `spawning` is at zero essentially always, so this now fires at almost any
    /// release; that is not a loosening, it is the point. The hold is still what
    /// makes "nothing is being started" true for the duration of the window.
    fn consider_hold(&self, g: &mut GateInner) -> bool {
        if g.admitting
            && g.spawning == 0
            && g.requests == 0
            && self.armed_flag.load(Ordering::SeqCst)
        {
            g.admitting = false;
            g.held_at = Some(Instant::now());
            return true;
        }
        false
    }

    /// Announce a held window and wake the watcher that will redeem it.
    fn after_release(&self, held: bool) {
        if held {
            tracing::warn!(
                "update: the daemon just went quiet and an approved binary is staged — admission \
                 is CLOSED and the restart fires now"
            );
            self.log_event("window_held", json!({}));
        }
        self.wake.notify_one();
    }

    /// A turn permit coming back is no longer a zero-crossing of the quiet
    /// condition — `turns` is not in it. `consider_hold` is still called because
    /// a turn ending is a perfectly good moment to notice that the daemon is
    /// quiet and a candidate is staged, and because dropping the call would make
    /// the hold depend on a spawn or a request happening to complete.
    fn release_turn(&self, thread: Option<String>) {
        let held = {
            let mut g = self.lock();
            g.turns = g.turns.saturating_sub(1);
            if let Some(t) = thread {
                if let Some(n) = g.turn_threads.get_mut(&t) {
                    *n -= 1;
                    if *n == 0 {
                        g.turn_threads.remove(&t);
                    }
                }
            }
            self.consider_hold(&mut g)
        };
        self.after_release(held);
    }

    fn release_spawn(&self) {
        let held = {
            let mut g = self.lock();
            g.spawning = g.spawning.saturating_sub(1);
            self.consider_hold(&mut g)
        };
        self.after_release(held);
    }

    fn release_request(&self) {
        let held = {
            let mut g = self.lock();
            g.requests = g.requests.saturating_sub(1);
            self.consider_hold(&mut g)
        };
        self.after_release(held);
    }

    /// Is a window being held open at a zero-crossing, waiting to be redeemed?
    pub fn window_held(&self) -> bool {
        self.lock().held_at.is_some()
    }

    // ── the window ───────────────────────────────────────────────────────

    /// **The quiet window.** Verify quiet, CLOSE admission, and run `act` —
    /// all inside one hold of `inner`.
    ///
    /// `act` must not touch this gate (it would deadlock), and in production it
    /// does not return: it `execv`s. A returned `Err` means the exec did not
    /// happen, and admission is reopened before we hand the reason back.
    ///
    /// This is the whole race argument. There is no line between the quiet
    /// check and `act()` that releases the lock, so `admit_turn` cannot succeed
    /// in between: it does not "usually lose the race", it cannot run.
    pub fn with_quiet_window<F>(&self, act: F) -> WindowOutcome
    where
        F: FnOnce() -> std::result::Result<(), String>,
    {
        let mut g = self.lock();
        let blockers = Blockers {
            turns: g.turns,
            spawning: g.spawning,
            requests: g.requests,
        };
        if !blockers.quiet() {
            // A window HELD at a zero-crossing that has since been broken —
            // `enter_request` or `enter_spawn`, since admission is shut and
            // neither of those is refusable. Give the daemon back rather than
            // leaving it closed until the heal.
            if g.held_at.is_some() {
                g.admitting = true;
                g.held_at = None;
            }
            return WindowOutcome::Busy(blockers);
        }
        // From here the closure is no longer a HOLD, it is a firing window: it
        // is not eligible for the self-heal, because after `act()` succeeds this
        // process does not exist.
        g.admitting = false;
        g.held_at = None;
        match act() {
            Ok(()) => WindowOutcome::Fired,
            Err(why) => {
                g.admitting = true;
                WindowOutcome::Refused(why)
            }
        }
    }

    /// Reopen admission after a [`WindowOutcome::Fired`] that was produced by a
    /// test hook rather than a real `execv`. Tests only — in production nothing
    /// executes after `Fired`.
    pub fn reopen_for_test(&self) {
        let mut g = self.lock();
        g.admitting = true;
        g.held_at = None;
    }

    /// Install a stand-in for `execv`. Tests only.
    pub fn set_exec_hook(&self, hook: ExecHook) {
        *self.exec_hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    // ── queueing ─────────────────────────────────────────────────────────

    pub fn pending(&self) -> Option<PendingRestart> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Queue a restart, durably.
    ///
    /// A second `new_binary` reason with a DIFFERENT identity replaces the
    /// first: a restart should always land on the newest build, and the reason
    /// is the thing we are waiting to become. An operator reason is never
    /// silently overwritten.
    pub fn request_restart(&self, reason: RestartReason) {
        let binary = self
            .exe
            .clone()
            .unwrap_or_else(|| PathBuf::from("<unknown>"));
        let next = PendingRestart::new(&reason, &binary);
        let mut replaced = false;
        {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = p.as_ref() {
                let both_binary = existing.kind == "new_binary" && next.kind == "new_binary";
                if both_binary && existing.identity != next.identity {
                    replaced = true;
                } else {
                    tracing::debug!(
                        existing = %existing.note,
                        "update: a restart is already queued — keeping it"
                    );
                    return;
                }
            }
            *p = Some(next.clone());
        }
        self.write_pending(Some(&next));
        // A fresh build clears any refusal backoff: the thing that was refused
        // is not the thing on disk now. It also DISARMS: whatever was approved
        // and staged was approved for the previous identity, and a staged
        // candidate must never be one build behind the reason it is staged for.
        self.disarm();
        *self.retry_after.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.last_defer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.escalated_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
        tracing::info!(
            reason = %next.note, kind = %next.kind,
            replaced,
            "update: RESTART QUEUED — it will fire at the first moment nothing is \
             running and nothing can start"
        );
        self.log_event(
            if replaced { "requeued" } else { "queued" },
            json!({ "kind": next.kind, "note": next.note, "identity": next.identity }),
        );
        self.wake.notify_one();
    }

    fn clear_pending(&self, why: &str) {
        let had = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .is_some();
        self.disarm();
        if had {
            tracing::info!(why, "update: the queued restart is satisfied — clearing it");
            self.log_event("satisfied", json!({ "why": why }));
        }
        self.write_pending(None);
    }

    fn write_pending(&self, p: Option<&PendingRestart>) {
        let path = self.pending_path();
        match p {
            None => {
                let _ = std::fs::remove_file(&path);
            }
            Some(p) => {
                let Ok(text) = serde_json::to_string_pretty(p) else {
                    return;
                };
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    if let Err(e) = std::fs::rename(&tmp, &path) {
                        tracing::warn!(error = %e, "update: could not install the pending record");
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
        }
    }

    fn read_pending(&self) -> Option<PendingRestart> {
        let text = std::fs::read_to_string(self.pending_path()).ok()?;
        match serde_json::from_str(&text) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "update: the pending-restart record is unreadable");
                None
            }
        }
    }

    // ── logging ──────────────────────────────────────────────────────────

    /// One JSON line per decision. A failure to write must never stop a restart.
    ///
    /// **ONE `write_all`, NOT `writeln!`.** This used to be `writeln!(f,
    /// "{line}")` on a raw `File`. `write_fmt` on an unbuffered `File` turns
    /// every formatting fragment into its own `write` syscall, and
    /// `serde_json::Value`'s `Display` emits token by token — so a single
    /// logged line left the process as a dozen-odd small `O_APPEND` writes.
    /// `O_APPEND` makes each of those writes atomic against each other, but it
    /// says nothing about the group, so two writers interleaved INSIDE one
    /// line, character by character.
    ///
    /// It corrupted this ledger on 2026-08-04 and again on 2026-08-20 — and the
    /// second time it ate the two `boot` lines (the incumbent's and the losing
    /// duplicate daemon's, written in the same millisecond) that were the whole
    /// evidence of the duplicate-daemon race. A diagnostic that destroys the
    /// record it exists to keep, exactly when the thing it is watching happens,
    /// is worse than no diagnostic.
    ///
    /// Building the line into a `String` first makes it one write, and one
    /// `O_APPEND` write under `PIPE_BUF`-ish sizes is indivisible.
    /// `tests/daemon_race.rs::two_writers_never_interleave_a_ledger_line`
    /// proves it at N = 4 concurrent writers.
    pub fn log_event(&self, event: &str, extra: Value) {
        use std::io::Write;
        let mut line = json!({
            "at": now_rfc3339(),
            "event": event,
            "pid": std::process::id(),
            "binary": self.exe.as_ref().map(|p| p.display().to_string()),
        });
        if let (Some(obj), Some(more)) = (line.as_object_mut(), extra.as_object()) {
            for (k, v) in more {
                obj.insert(k.clone(), v.clone());
            }
        }
        // Serialize into a String FIRST — the whole point. Nothing reaches the
        // fd until the line is complete and carries its own newline.
        let mut buf = line.to_string();
        buf.push('\n');
        let _ = std::fs::create_dir_all(&self.data_dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())
        {
            let _ = f.write_all(buf.as_bytes());
        }
    }

    /// One line per CHANGE of blockers — and, unlike the first version of this,
    /// one line every [`DEFER_RELOG`] even when nothing changes.
    ///
    /// The dedup-forever this replaces is why an hour of starvation left a
    /// ledger holding seven `deferred` lines (one per rebuild, because a requeue
    /// reset the dedup) and nothing at all in between. It read as "it deferred
    /// seven times"; it had in fact deferred several hundred times.
    fn log_defer(&self, b: Blockers) {
        let sig = b.describe();
        {
            let mut last = self.last_defer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((prev, at)) = last.as_ref() {
                if prev == &sig && at.elapsed() < DEFER_RELOG {
                    return;
                }
            }
            *last = Some((sig.clone(), Instant::now()));
        }
        let age = self.pending().and_then(|p| pending_age(&p));
        tracing::info!(
            // REPORTED, NOT A BLOCKER — an operator reading this line needs to
            // know what the daemon was carrying, and a `turns` that is high
            // while a restart defers is now information, not a cause.
            turns = b.turns,
            spawning = b.spawning,
            requests = b.requests,
            pending_secs = age.map(|d| d.as_secs()),
            "update: a restart is queued but the daemon is not quiet — deferring"
        );
        self.log_event(
            "deferred",
            json!({
                "turns": b.turns,
                "spawning": b.spawning,
                "requests": b.requests,
                "blockers": sig,
                "pending_secs": age.map(|d| d.as_secs()),
                "armed": self.armed_flag.load(Ordering::SeqCst),
            }),
        );
    }

    /// **THE STARVATION BOUND.** A restart that has been pending past
    /// [`escalate_after`] is not waiting for a quiet moment any more, it is
    /// starving, and it says so at ERROR every [`escalate_every`].
    ///
    /// This exists because the first version of this module deferred for an
    /// hour without a single loud line, and the only way anyone found out was a
    /// human comparing a running pid against a file on disk.
    fn check_starvation(&self) {
        let Some(p) = self.pending() else { return };
        let Some(age) = pending_age(&p) else { return };
        if age < escalate_after() {
            return;
        }
        {
            let mut last = self.escalated_at.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = *last {
                if t.elapsed() < escalate_every() {
                    return;
                }
            }
            *last = Some(Instant::now());
        }
        let b = self.peek();
        let armed = self.armed_flag.load(Ordering::SeqCst);
        tracing::error!(
            pending_secs = age.as_secs(),
            queued_at = %p.queued_at,
            note = %p.note,
            // Both are printed, and the distinction is the diagnosis: a
            // `starving` line with a high `turns` and `spawning=0` used to mean
            // "waiting for a fleet-wide idle that never comes" and now cannot
            // happen at all. One with `spawning` stuck above zero means a spawn
            // permit leaked, which is a real bug and a different one.
            turns = b.turns,
            spawning = b.spawning,
            requests = b.requests,
            armed,
            "update: A RESTART HAS BEEN PENDING FOR {} MINUTES AND STILL HAS NOT HAPPENED. This \
             daemon is STARVING on a quiet moment that never arrives, and it is still running the \
             OLD binary. Blockers: {}.",
            age.as_secs() / 60,
            b.describe(),
        );
        self.log_event(
            "starving",
            json!({
                "pending_secs": age.as_secs(),
                "queued_at": p.queued_at,
                "note": p.note,
                "turns": b.turns,
                "spawning": b.spawning,
                "requests": b.requests,
                "armed": armed,
            }),
        );
    }

    // ── boot ─────────────────────────────────────────────────────────────

    /// Run once at daemon start, before serving.
    ///
    /// Reconciles the durable pending record against the image we actually
    /// booted from, then promotes that image to last-good.
    pub async fn on_boot(&self) {
        let _ = std::fs::create_dir_all(&self.data_dir);
        let mut boot_pending: Option<PendingRestart> = None;
        let mut boot_satisfied = false;
        if let Some(rec) = self.read_pending() {
            // Adopt it BEFORE deciding, so `clear_pending` has something to
            // clear and the decision reaches the log either way.
            *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(rec.clone());
            let satisfied = match (&rec.identity, &self.boot_identity) {
                (Some(want), Some(have)) => want == have,
                // An operator-requested restart has no identity to match; the
                // fact that we are booting at all satisfies it.
                (None, _) => true,
                (Some(_), None) => false,
            };
            if satisfied {
                self.clear_pending("this boot carries the identity the record was waiting for");
            } else {
                tracing::warn!(
                    queued_at = %rec.queued_at, note = %rec.note,
                    "update: a queued restart survived a daemon exit and is still pending"
                );
            }
            boot_satisfied = satisfied;
            boot_pending = Some(rec);
        }
        self.log_event(
            "boot",
            json!({ "identity": self.boot_identity, "pending": self.pending().is_some() }),
        );
        // ── WHAT THIS BOOT HAS TO SAY FOR ITSELF ──────────────────────────
        //
        // Computed here, where the pending record has just been reconciled
        // against the identity we actually booted with, because that
        // reconciliation IS the evidence. It is only STORED: `on_boot` runs
        // before the server is listening, so there is provably no socket to
        // broadcast down yet. The `boot` shape (`shapes::boot`) reads it back
        // through `boot_note()` and exports it to every subscriber, at any
        // later time; the client — which is the only party that knows whether
        // it saw a previous `boot_id` — decides whether it is news. (Until R2.S4
        // the legacy firehose ALSO replayed it once per connection, from
        // `server::drive` and `chassis::ThreadsControl`; both went with it.)
        let note = self.compute_boot_note(boot_pending.as_ref(), boot_satisfied);
        tracing::info!(
            reason = %note.reason, detail = %note.detail,
            "lifecycle: this boot's account of itself"
        );
        *self.boot_note.lock().unwrap_or_else(|e| e.into_inner()) = Some(note);
        self.keep_last_good().await;
    }

    /// Record the running image as the binary known to boot — because it did.
    ///
    /// Deliberately refuses to promote an image that does not pass the same
    /// checks a candidate must pass; a fallback that is itself broken is worse
    /// than no fallback, because it hides the problem behind a second failure.
    pub async fn keep_last_good(&self) {
        // Disarmed process (no boot identity — see `new`): there is nothing
        // here worth calling "the daemon known to boot".
        if self.boot_identity.is_none() {
            return;
        }
        let Some(exe) = self.exe.as_deref() else {
            tracing::warn!("update: no current_exe — last-good copy skipped");
            return;
        };
        if !usable(exe).await {
            tracing::warn!(
                binary = %exe.display(),
                "update: the running image does not pass its own preflight — NOT promoting it \
                 to last-good"
            );
            return;
        }
        let dest = self.last_good_path();

        // ── IS THE LAST-GOOD COPY ALREADY THIS VERSION? ───────────────────
        //
        // The version is the COMMIT. See the "last-good is named by a commit"
        // section in the module header for why, and for why the byte comparison
        // survives as the fallback rather than being deleted.
        let mine = crate::image::build_commit();
        let theirs = commit_of(&dest);
        let verdict = last_good_verdict(
            mine,
            theirs.as_deref(),
            self.boot_identity.as_deref(),
            // Only asked for when the commits cannot settle it: it is a
            // `codesign` subprocess, and on the healthy path the commits agree.
            || binary_identity(&dest),
        );
        if verdict == LastGood::AlreadyThis {
            return;
        }

        // ── CLEAR THE WAY ─────────────────────────────────────────────────
        //
        // A temp left by an earlier boot may be IMMUTABLE and therefore
        // un-openable, which would fail the copy below before it started. This
        // is the self-heal: see the module header.
        let tmp = dest.with_extension("copying");
        if tmp.exists() {
            let _ = clear_file_flags(&tmp);
            match std::fs::remove_file(&tmp) {
                Ok(()) => tracing::info!(
                    path = %tmp.display(),
                    "update: cleared a stale last-good temp left by an earlier boot"
                ),
                Err(e) => {
                    tracing::warn!(
                        error = %e, path = %tmp.display(),
                        "update: a stale last-good temp is in the way and cannot be removed — \
                         last-good is NOT tracking the running image"
                    );
                    return;
                }
            }
        }

        // Write beside the destination and rename: a crash mid-copy must never
        // leave a truncated "known good" binary behind.
        match std::fs::copy(exe, &tmp) {
            Ok(_) => {
                // `fs::copy` carries macOS file FLAGS across, not just the
                // bytes and mode — measured, see the module header. The deploy
                // path is deliberately `uchg`, so without this the temp is born
                // immutable and can be neither renamed nor removed.
                if let Err(e) = clear_file_flags(&tmp) {
                    tracing::warn!(
                        error = %e, path = %tmp.display(),
                        "update: could not clear file flags on the last-good temp"
                    );
                }
                let _ = set_executable(&tmp);
                match std::fs::rename(&tmp, &dest) {
                    Ok(_) => {
                        tracing::info!(
                            path = %dest.display(), commit = ?mine,
                            "update: last-good binary updated"
                        );
                        self.log_event(
                            "last_good",
                            json!({ "identity": self.boot_identity, "commit": mine }),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "update: could not install the last-good binary");
                        let _ = clear_file_flags(&tmp);
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "update: could not copy the last-good binary"),
        }
    }

    // ── the watch ────────────────────────────────────────────────────────

    /// Has a SIGNED image with a new code identity landed at our own path?
    ///
    /// Cheap: one `stat`, and a `codesign` call only when the stat moved. mtime
    /// is a HINT that triggers the check; the identity is what decides. A
    /// `touch`, or a rebuild that produced identical bytes, never queues
    /// anything.
    ///
    /// **A bare `cargo build` / `cargo check` is a NO-OP here** — see "The
    /// absorb signal" in the module header, and [`absorbable_identity`]. Cargo's
    /// output is ad-hoc-signed, so `absorbable_identity` is `None` and we return
    /// having done nothing: no queue, no clear, no log line. The daemon keeps
    /// running the code it is running until somebody runs
    /// `lib/threads/scripts/build-threads.sh`, which signs.
    ///
    /// A later reader will be tempted to "fix" the early return so an unsigned
    /// build at least clears a stale pending record, or logs. **Don't.** Many
    /// agents share this build tree; if an unsigned image can move any state in
    /// this module, a compile check is no longer a no-op, which is the entire
    /// requirement.
    pub fn scan_binary(&self) {
        let (Some(exe), Some(boot)) = (self.exe.as_deref(), self.boot_identity.as_deref()) else {
            return;
        };
        let Ok(m) = std::fs::metadata(exe) else {
            tracing::debug!(binary = %exe.display(), "update: own binary is not readable right now");
            return;
        };
        let stat = (m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH));
        {
            let mut last = self.last_stat.lock().unwrap_or_else(|e| e.into_inner());
            if *last == Some(stat) {
                return;
            }
            *last = Some(stat);
        }
        // NOT `binary_identity`. An unsigned image is not a deploy at all, and
        // the difference between the two functions is the whole feature.
        let Some(identity) = absorbable_identity(exe) else {
            tracing::debug!(
                binary = %exe.display(),
                "update: own binary changed but is not signed for deploy — ignoring \
                 (a compile check is a no-op; run scripts/build-threads.sh to deploy)"
            );
            return;
        };
        if identity == boot {
            // A rebuild that reverted to our own bytes: whatever was queued for
            // a binary change is moot.
            if self
                .pending()
                .map(|p| p.kind == "new_binary")
                .unwrap_or(false)
            {
                self.clear_pending("the on-disk image is the one we are already running");
            }
            tracing::debug!("update: own binary touched but the code identity is unchanged");
            return;
        }
        self.request_restart(RestartReason::NewBinary { identity });
    }

    // ── preflight + fire ─────────────────────────────────────────────────

    /// Everything expensive, done OUTSIDE the window: verify the candidate,
    /// restore last-good over it if it is bad, and refuse outright if there is
    /// nothing usable to become.
    pub async fn prepare(&self) -> std::result::Result<Ready, String> {
        if self.boot_identity.is_none() {
            return Err(
                "this process is not the daemon binary — the update protocol is disarmed".into(),
            );
        }
        let exe = self
            .exe
            .clone()
            .ok_or_else(|| "no resolvable executable path".to_string())?;
        let good = self.last_good_path();
        let on_disk_ok = usable(&exe).await;
        let last_good_ok = !on_disk_ok && usable(&good).await;
        let mut decision = decide(on_disk_ok, last_good_ok);

        if decision == Preflight::Restored {
            tracing::error!(
                binary = %exe.display(), last_good = %good.display(),
                "update: the binary we were about to restart into is missing, truncated or \
                 does not run — restoring the last known-good image over it"
            );
            let tmp = exe.with_extension("restoring");
            let restored = std::fs::copy(&good, &tmp)
                .and_then(|_| {
                    set_executable(&tmp)?;
                    std::fs::rename(&tmp, &exe)
                })
                .is_ok();
            if !restored {
                let _ = std::fs::remove_file(&tmp);
                decision = Preflight::NoFallback;
            } else if !usable(&exe).await {
                decision = Preflight::NoFallback;
            }
        }

        self.log_event(
            "preflight",
            json!({ "preflight": decision.as_str(), "on_disk_ok": on_disk_ok }),
        );

        if decision == Preflight::NoFallback {
            // The wording used to end "…this daemon has no supervisor, so
            // exiting would leave you with no daemon at all". There IS a
            // supervisor now (see `decide`), and a message that argues from a
            // fact that stopped being true is worse than no message. The
            // refusal's real reason is the one stated here: nothing we could
            // exec into is known to run, and refusing an `execv` costs nothing.
            let why = format!(
                "REFUSING to restart: {} does not pass preflight and there is no usable \
                 last-good copy at {}. Staying on the running build — refusing an execv \
                 leaves this daemon serving; firing one into an image that does not run \
                 is the only way to end up with no daemon at all.",
                exe.display(),
                good.display()
            );
            tracing::error!("{why}");
            self.log_event("refused", json!({ "why": why }));
            return Err(why);
        }

        let identity =
            binary_identity(&exe).ok_or_else(|| "could not fingerprint the candidate".to_string())?;
        let banner = self_check(&exe).await?;
        Ok(Ready {
            binary: exe,
            identity,
            preflight: decision,
            banner,
        })
    }

    /// Take the window and become `ready`.
    ///
    /// In production this does not return on success. The final identity
    /// re-check happens INSIDE the window, so a build that landed between the
    /// preflight and the window aborts rather than going live unverified.
    pub fn fire(&self, ready: &Ready) -> WindowOutcome {
        let outcome = self.with_quiet_window(|| {
            let now = binary_identity(&ready.binary);
            if now.as_deref() != Some(ready.identity.as_str()) {
                return Err(format!(
                    "the binary changed between preflight and the quiet window \
                     (approved {}, found {})",
                    ready.identity,
                    now.as_deref().unwrap_or("<unreadable>")
                ));
            }
            if let Some(hook) = self
                .exec_hook
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                return hook(&ready.binary);
            }
            Err(exec_self(&ready.binary))
        });
        match &outcome {
            WindowOutcome::Fired => {
                self.log_event(
                    "firing",
                    json!({
                        "identity": ready.identity,
                        "preflight": ready.preflight.as_str(),
                        "banner": ready.banner,
                    }),
                );
            }
            WindowOutcome::Busy(b) => self.log_defer(*b),
            WindowOutcome::Refused(why) => {
                tracing::error!(why, "update: reached the quiet window and did NOT restart");
                self.log_event("refused", json!({ "why": why }));
            }
        }
        outcome
    }

    /// The candidate approved and staged by [`UpdateGate::arm`], if any.
    pub fn armed(&self) -> Option<Ready> {
        self.armed.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Throw away a staged candidate. Called whenever the thing it was staged
    /// for stops being true.
    fn disarm(&self) {
        *self.armed.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.armed_flag.store(false, Ordering::SeqCst);
    }

    /// **ARM WHILE BUSY — the half of the starvation fix that costs nothing.**
    ///
    /// Everything expensive about a restart (two subprocess `--self-check`
    /// spawns, a SHA-256 of a 27 MB binary, possibly restoring last-good over a
    /// bad build) happens HERE, with no requirement that the daemon be quiet.
    /// A busy daemon is the ideal time: the work is off the critical path
    /// entirely, and what remains at the quiet instant is one hash and an
    /// `execv`.
    ///
    /// This is the exact thing that was starving. The ledger recorded a
    /// successful `preflight` and then no `firing` — the watcher had found the
    /// daemon quiet, spent the quiet moment preflighting, and found it busy
    /// again when it finally asked for the window.
    ///
    /// Idempotent, and cheap after the first success. `Err` is a refusal to
    /// restart at all (nothing usable to become), which the caller reports.
    pub async fn arm(&self) -> std::result::Result<(), String> {
        if self.armed_flag.load(Ordering::SeqCst) {
            return Ok(());
        }
        {
            let hold = self.retry_after.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = *hold {
                if Instant::now() < t {
                    return Ok(());
                }
            }
        }
        match self.prepare().await {
            Ok(ready) => {
                tracing::info!(
                    identity = %ready.identity,
                    preflight = ready.preflight.as_str(),
                    "update: the new binary is APPROVED and STAGED — it will be exec'd at the \
                     instant the last turn ends, with admission already closed"
                );
                self.log_event(
                    "armed",
                    json!({ "identity": ready.identity, "preflight": ready.preflight.as_str() }),
                );
                // ── SAY IT NOW, NOT AT THE EXEC ───────────────────────────
                //
                // `armed` is the earliest instant at which "this daemon is
                // restarting" is a COMMITTED fact rather than a possibility:
                // the candidate has passed its own `--self-check`, it is
                // staged, and everything left is one hash and an `execv`.
                //
                // It is also the LAST instant with any slack in it. `fire`
                // runs inside the quiet window holding the gate mutex and
                // ends in `execv`, which replaces this process — every thread,
                // every socket, mid-flush. A frame broadcast there would be
                // sitting in the fan-out ring, unsent, at the moment the
                // process it belongs to stops existing. So the announcement
                // goes out here, seconds to minutes ahead, while every
                // connection is live and idle enough to drain it.
                //
                // A missed one is not a silent restart: the client also sees
                // the socket drop and a new `boot_id` on reconnect, which is
                // independent evidence that this happened. See `on_boot`.
                let (reason, detail) = self.going_down_reason(ready.preflight);
                self.announce(PHASE_GOING_DOWN, reason, detail);
                *self.armed.lock().unwrap_or_else(|e| e.into_inner()) = Some(ready);
                self.armed_flag.store(true, Ordering::SeqCst);
                // A daemon that is ALREADY quiet has no zero-crossing coming;
                // poke the watcher so it does not wait for one.
                self.wake.notify_one();
                Ok(())
            }
            Err(why) => {
                *self.retry_after.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Instant::now() + REFUSAL_BACKOFF);
                Err(why)
            }
        }
    }

    /// Redeem a window that [`UpdateGate::consider_hold`] is holding open.
    ///
    /// Deliberately does NOTHING expensive: admission is shut while this runs,
    /// so every microsecond here is a turn refused. The candidate was approved
    /// while the daemon was busy; all that is left is the final identity
    /// re-check inside the window, and the `execv`.
    pub fn fire_held(&self) -> Option<WindowOutcome> {
        if !self.window_held() {
            return None;
        }
        let ready = self.armed()?;
        let outcome = self.fire(&ready);
        if let WindowOutcome::Refused(_) = outcome {
            // Whatever was staged is not exec'able; drop it so `consider_hold`
            // stops closing windows for it, and let `arm` decide again.
            self.disarm();
            *self.retry_after.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Instant::now() + REFUSAL_BACKOFF);
        }
        Some(outcome)
    }

    /// One end-to-end attempt: escalate if starving, ARM (expensive, done
    /// whether or not we are quiet), then peek and fire.
    ///
    /// Note the order. The preflight no longer waits for quiet — that ordering
    /// is what starved, because it put a hundred milliseconds of subprocess work
    /// between observing the quiet moment and using it.
    pub async fn attempt(&self) -> Option<WindowOutcome> {
        self.pending()?;
        self.check_starvation();
        if let Err(why) = self.arm().await {
            return Some(WindowOutcome::Refused(why));
        }
        let b = self.peek();
        if !b.quiet() {
            self.log_defer(b);
            return Some(WindowOutcome::Busy(b));
        }
        let ready = self.armed()?;
        // The log line the operator asked for: say it before it happens, because
        // afterwards this process does not exist.
        tracing::warn!(
            identity = %ready.identity,
            preflight = ready.preflight.as_str(),
            "update: QUIET — nothing is running and admission is about to close; \
             restarting into the new binary now"
        );
        let outcome = self.fire(&ready);
        if let WindowOutcome::Refused(_) = outcome {
            self.disarm();
            *self.retry_after.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Instant::now() + REFUSAL_BACKOFF);
        }
        Some(outcome)
    }

    /// Should the watcher arm itself for this process?
    ///
    /// Only for a binary literally named `threads`, and never when
    /// `THREADS_UPDATE_WATCH=off`. A `cargo test` harness is named after its
    /// test target, so the watcher can never fire inside a test process — the
    /// tests drive [`attempt`](Self::attempt) and [`fire`](Self::fire) directly
    /// against fixture binaries instead.
    pub fn watch_wanted(&self) -> bool {
        if std::env::var("THREADS_UPDATE_WATCH").ok().as_deref() == Some("off") {
            return false;
        }
        self.exe
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n == DAEMON_EXE_NAME)
            .unwrap_or(false)
            && self.boot_identity.is_some()
    }

    /// Start the binary watcher + restart executor. Idempotent.
    ///
    /// Event-driven where it matters: every released permit pokes `wake`, so the
    /// restart fires the moment the last turn ends. The interval is the floor
    /// that notices a new binary landing on disk.
    pub fn spawn_watcher(self: &Arc<Self>) {
        if self.watcher_started.swap(true, Ordering::SeqCst) {
            return;
        }
        if !self.watch_wanted() {
            tracing::info!(
                binary = ?self.exe,
                "update: self-update watcher NOT armed for this process"
            );
            return;
        }
        tracing::info!(
            binary = ?self.exe, identity = ?self.boot_identity,
            signed_for_deploy = self.exe.as_deref().map(is_stably_signed).unwrap_or(false),
            "update: watching our own binary for a SIGNED build (an unsigned cargo \
             build is ignored — run lib/threads/scripts/build-threads.sh to deploy)"
        );
        let gate = self.clone();
        let poll = Duration::from_millis(env_ms("THREADS_UPDATE_POLL_MS", 5_000));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = gate.wake.notified() => {}
                    _ = tokio::time::sleep(poll) => {}
                }
                // A HELD WINDOW FIRST, and before anything that could cost time
                // or yield. `consider_hold` shut admission the instant the last
                // permit came back; the daemon is refusing turns until this
                // returns, and on success it does not return.
                if gate.window_held() {
                    gate.fire_held();
                }
                gate.scan_binary();
                gate.attempt().await;
            }
        });
    }
}

/// A permit to run ONE turn — the ADMISSION grant, and the `turns` counter that
/// every ledger line reports.
///
/// **Holding one of these no longer keeps the daemon from restarting.** It used
/// to, and the module header records what that cost. A running turn survives an
/// `execv` and is re-adopted at boot; the only unrecoverable window is
/// [`SpawnPermit`]'s.
pub struct TurnPermit {
    gate: Option<Arc<UpdateGate>>,
    /// The thread this turn runs on, when admitted by name (R9.C7b).
    thread: Option<String>,
}

impl TurnPermit {
    /// Give the permit back now rather than at drop — `TurnLock::release` calls
    /// this at the top of `finish_turn`, so the last turn ending frees the
    /// daemon immediately instead of one stack frame later.
    pub fn release(&mut self) {
        if let Some(g) = self.gate.take() {
            g.release_turn(self.thread.take());
        }
    }
}

impl Drop for TurnPermit {
    fn drop(&mut self) {
        self.release();
    }
}

/// **The quiet condition's only turn-side term.** Alive exactly between
/// `cmd.spawn()` and the `upsert_running_turn` that makes the child recoverable.
/// While any of these exists the daemon is not quiet and no restart window can
/// open — because an `execv` here would strand a live `claude` process that
/// nothing on disk names.
pub struct SpawnPermit {
    gate: Option<Arc<UpdateGate>>,
}

impl SpawnPermit {
    /// Give the permit back now rather than at drop, so the window closes on the
    /// instruction after the row is written rather than at the end of a scope
    /// that also spawns readers and waits on a child.
    pub fn release(&mut self) {
        if let Some(g) = self.gate.take() {
            g.release_spawn();
        }
    }
}

impl Drop for SpawnPermit {
    fn drop(&mut self) {
        self.release();
    }
}

/// A permit for one WS request being served.
pub struct RequestPermit {
    gate: Arc<UpdateGate>,
}

impl Drop for RequestPermit {
    fn drop(&mut self) {
        self.gate.release_request();
    }
}

/// Does the last-good copy already stand for the image that just booted?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastGood {
    /// Nothing to do. The copy on disk already represents this version.
    AlreadyThis,
    /// Install the running image over it.
    Promote,
}

/// THE DECISION, with no filesystem in it — whether the last-good copy already
/// stands for the running image.
///
/// Pure and separate from [`UpdateGate::keep_last_good`] because this is the part
/// that ARBITRATES, and it has to be right in cases a live machine will not
/// produce on demand: an image with no commit, an image from a dirty tree, and
/// two images that disagree. A test that can only reach the combination this
/// workspace happens to build today is not evidence about the others.
///
/// The rule, in one sentence: **a commit decides it when both sides have one,
/// and silence decides nothing.**
///
/// * Both commits present and equal — `AlreadyThis`. The version is the commit,
///   so two artifacts of the same commit are the same version even if a rebuild
///   gave them different bytes.
/// * Both present and different — `Promote`. A real version change.
/// * Either absent — fall through to `bytes`, the TOTAL identity, which is
///   precisely what this comparison was before a commit was the name of
///   anything. An image that predates `build.rs`, or one built from a dirty
///   tree, is not refused and is not assumed stale; it is compared the old way.
///
/// `bytes` is a closure because computing the far side's byte identity costs a
/// `codesign` subprocess, and on the healthy path the commits settle it first.
pub fn last_good_verdict(
    mine_commit: Option<&str>,
    theirs_commit: Option<&str>,
    mine_bytes: Option<&str>,
    theirs_bytes: impl FnOnce() -> Option<String>,
) -> LastGood {
    match (mine_commit, theirs_commit) {
        (Some(a), Some(b)) => {
            if a == b {
                LastGood::AlreadyThis
            } else {
                LastGood::Promote
            }
        }
        _ => {
            // Two unreadable images are not thereby "the same": `None == None`
            // would promote nothing for ever, so an absent byte identity on
            // either side falls to `Promote` and the copy is retried.
            match (mine_bytes, theirs_bytes()) {
                (Some(a), Some(b)) if a == b => LastGood::AlreadyThis,
                _ => LastGood::Promote,
            }
        }
    }
}

/// Clear every macOS file flag on `p` — notably `uchg`, the user immutable flag.
///
/// Needed because [`std::fs::copy`] on macOS copies *flags*, not only bytes and
/// mode: it goes through `copyfile`/`fcopyfile`, whose metadata set includes
/// `st_flags`. The deploy path is held `uchg` on purpose (see
/// `lib/threads/build.rs` — it is the only guard that survives a fully cached
/// cargo uplift), so a plain copy of the running image produces an immutable
/// temp, and an immutable file can be neither renamed nor unlinked.
///
/// Deliberately a no-op-shaped call rather than a conditional one: asking
/// whether the flags need clearing costs a `stat` and an `if`, and clearing
/// flags that are already zero costs nothing and cannot be wrong.
fn clear_file_flags(p: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has a NUL"))?;
    // SAFETY: `c` is a NUL-terminated C string that outlives the call, and
    // `chflags` reads it and nothing else.
    if unsafe { libc::chflags(c.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The commit a binary ON DISK was built from, by asking the binary itself —
/// `None` when it cannot say, or when its answer does not name its own bytes.
///
/// Asks rather than remembers, which is the whole point. A sidecar file
/// recording "last-good is commit X" is a second place the truth lives, and it
/// can be wrong about the file it sits next to — which is the defect class this
/// function exists to retire, not to relocate. `--build-stamp` is answered by
/// the image out of a constant its own `build.rs` compiled in, so the artifact
/// carries its version rather than being labelled with it.
///
/// `dirty=1` reads as `None`: the tree had uncommitted compiled inputs, so the
/// commit does not identify these bytes. Same rule as
/// [`crate::image::build_commit`], applied to the other side of the comparison,
/// because a rule that holds for one image and not the other is not a rule.
///
/// This is the same read-back `promote` performs before it signs anything: the
/// precedent for taking a commit off a product rather than off the build that
/// claimed to produce it.
pub fn commit_of(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let out = std::process::Command::new(path).arg("--build-stamp").output().ok()?;
    if !out.status.success() {
        return None;
    }
    // A flat `k=v` line — see `image::build_stamp_line`. Parsed field by field
    // rather than by position: a later field added in front of these must not
    // silently change what this function reads.
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| {
        text.split_whitespace()
            .find_map(|t| t.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
    };
    if field("dirty") != Some("0") {
        return None;
    }
    field("commit")
        .filter(|c| !c.is_empty() && *c != "unknown")
        .map(str::to_owned)
}

fn set_executable(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(p, perm)
}

/// Replace this process image with `path`, keeping pid, parent, argv and env.
///
/// Only returns when the exec FAILED — the returned string is why. That is the
/// property the whole come-back story rests on: a failed exec leaves the old
/// daemon running rather than leaving the operator with nothing.
fn exec_self(path: &Path) -> String {
    use std::os::unix::process::CommandExt;
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let err = std::process::Command::new(path).args(&args).exec();
    format!("exec of {} failed: {err}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_policy_refuses_when_there_is_nothing_good_to_become() {
        assert_eq!(decide(true, true), Preflight::Ok);
        assert_eq!(decide(true, false), Preflight::Ok);
        assert_eq!(decide(false, true), Preflight::Restored);
        // The divergence from the AUI: this means DON'T GO. Not because there
        // is no supervisor (there is one now) — because a refused execv leaves
        // us serving and a fired one into an unrunnable image does not.
        assert_eq!(decide(false, false), Preflight::NoFallback);
    }

    #[test]
    fn a_truncated_or_non_binary_file_is_never_plausible() {
        let dir = std::env::temp_dir().join(format!("threads-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tiny = dir.join("tiny");
        std::fs::write(&tiny, b"not a mach-o").unwrap();
        assert!(!plausible(&tiny));
        let text = dir.join("big-text");
        std::fs::write(&text, vec![b'a'; (MIN_BINARY_BYTES + 1) as usize]).unwrap();
        set_executable(&text).unwrap();
        assert!(!plausible(&text), "a big executable text file is not a binary");
        assert!(!plausible(&dir.join("absent")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The SHA-256 fallback arm of [`binary_identity`]: for a file `codesign`
    /// can report no CDHash for (here, four bytes that are not a Mach-O),
    /// identity is content and never mtime.
    /// R9.C7: `idle` is `quiet` AND no turn executing, and `/health`'s
    /// `restart_gate` carries both apart. Paired so a constant cannot pass: a
    /// running turn is quiet-but-not-idle, a spawn is neither, nothing is both.
    #[test]
    fn r9c7_restart_gate_idle_waits_for_turns_and_quiet_does_not() {
        let b = |turns, spawning, requests| Blockers { turns, spawning, requests };
        let gate = |x: Blockers| x.health_extras()["restart_gate"].clone();
        let running = gate(b(1, 0, 0));
        assert_eq!((running["quiet"].as_bool(), running["idle"].as_bool()), (Some(true), Some(false)), "{running}");
        assert_eq!(running["turns"], 1);
        let spawning = gate(b(0, 1, 0));
        assert_eq!((spawning["quiet"].as_bool(), spawning["idle"].as_bool()), (Some(false), Some(false)));
        let serving = gate(b(0, 0, 1));
        assert_eq!(serving["idle"].as_bool(), Some(false));
        let nothing = gate(b(0, 0, 0));
        assert_eq!((nothing["quiet"].as_bool(), nothing["idle"].as_bool()), (Some(true), Some(true)));
    }

    /// R9.C7c: a drain refuses every new admission (so new work stays queued),
    /// leaves permits already held alone, keeps its FIRST instant across a
    /// repeated request (the supervisor's cap is measured from it), shows on
    /// `/health`, and a cancel reopens admission. Paired: admission is open
    /// before the drain and after the cancel, so a constant refusal fails too.
    #[test]
    fn r9c7c_a_drain_refuses_admission_keeps_its_first_instant_and_a_cancel_reopens() {
        let dir = std::env::temp_dir().join(format!("threads-drain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gate = UpdateGate::new(dir.clone());
        let running = gate.admit_turn().expect("open before a drain");
        assert_eq!(gate.health_extras()["restart_gate"]["draining_since"], Value::Null);
        assert!(gate.begin_drain());
        let since = gate.draining_since().expect("draining");
        assert!(!gate.begin_drain(), "a repeat is not a new drain");
        assert_eq!(gate.draining_since(), Some(since), "the first instant is kept");
        assert!(gate.admit_turn().is_none(), "a drain admitted a new turn");
        assert!(!gate.admitting());
        assert_eq!(gate.peek().turns, 1, "the running turn's permit is untouched");
        let g = gate.health_extras()["restart_gate"].clone();
        assert_eq!((g["draining"].as_bool(), g["idle"].as_bool()), (Some(true), Some(false)), "{g}");
        assert!(g["draining_since"].is_u64(), "{g}");
        drop(running);
        assert_eq!(gate.health_extras()["restart_gate"]["idle"], true, "drained: idle");
        assert!(gate.end_drain());
        assert!(!gate.end_drain(), "nothing left to cancel");
        assert!(gate.admitting());
        assert!(gate.admit_turn().is_some(), "a cancelled drain admits again");
        assert_eq!(gate.health_extras()["restart_gate"]["draining_since"], Value::Null);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identity_is_content_not_mtime() {
        let dir = std::env::temp_dir().join(format!("threads-ident-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bin");
        std::fs::write(&p, b"aaaa").unwrap();
        let first = binary_identity(&p).unwrap();
        assert!(first.starts_with("sha256:"), "a non-Mach-O falls back: {first}");
        // Rewrite the same bytes: mtime moves, identity does not.
        std::fs::write(&p, b"aaaa").unwrap();
        assert_eq!(binary_identity(&p).as_deref(), Some(first.as_str()));
        std::fs::write(&p, b"bbbb").unwrap();
        assert_ne!(binary_identity(&p).as_deref(), Some(first.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing that is not stably signed is absorbable — including the cases
    /// that are not binaries at all. The default answer to "should we deploy
    /// this" is NO, so a `codesign` that errors, a missing file or a text file
    /// can never be read as a deploy signal.
    #[test]
    fn nothing_unsigned_is_ever_absorbable() {
        let dir = std::env::temp_dir().join(format!("threads-absorb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("not-a-binary");
        std::fs::write(&text, b"aaaa").unwrap();
        assert!(!is_stably_signed(&text));
        assert!(binary_identity(&text).is_some(), "but it still HAS a code identity");
        assert!(absorbable_identity(&text).is_none(), "it is just never a deploy");

        let absent = dir.join("nothing-here");
        assert!(!is_stably_signed(&absent));
        assert!(absorbable_identity(&absent).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
