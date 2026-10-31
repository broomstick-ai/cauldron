#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  tui (shell) conformance test — TERMINAL OWNERSHIP, proven on a real pty.
#
#  The two things this face can get wrong that nothing else in the repo can
#  catch: which modes it takes from the terminal, and whether it gives them
#  back. Both are byte-stream facts, so both are checked by reading the bytes
#  the binary actually writes to a pty — not by reading the source, and not by
#  trusting crossterm.
#
#  `core:writing-tools`: a green you have not forced to fail is not evidence.
#  Each assertion here is therefore run twice — once against the real binary,
#  and once against a binary deliberately broken in the way that assertion
#  exists to catch. The mutations edit COPIES of the sources, rebuild, probe,
#  and restore on every exit path including a signal.
#
#  It touches nothing outside this crate: no launchd job, no running process,
#  and NO DAEMON — the probe sets every variable the face reads to decide where
#  it connects (`TUI_THREADS_CHASSIS_PORT`/`TUI_THREADS_PORT` = the crate's own
#  fake daemon on a loopback port, `AUI_V2_URL` on port 1, `CAULDRON_BIN=/usr/bin/false`), so it exercises the terminal
#  without ever reaching the operator's threads daemon (3653); and it gives the
#  face a scratch HOME and no TMUX (R7.S5b), so it neither writes beside his
#  live face's `~/.tui`/`~/.cauldron` nor queries his tmux server. Until R7.S5 it
#  set only `TUI_THREADS_URL`, which nothing reads, and this claim was false;
#  `tests/drive_isolation.rs` now pins the set. That mattered from build step 2
#  onward: before the transport existed this suite could not have touched a
#  daemon if it tried, and afterwards it would have subscribed to a live
#  firehose on every run.
#
#  **`transport_conformance_test.sh` IS RETIRED (R7.S3, HOLDS R22).** It
#  forced mutations of `src/transport.rs`'s legacy hand-rolled transport —
#  `supervise`, `session`, `handle_frame`, `BootState`, `ResyncKind`,
#  `start_resync`, `Cursors`, `park`, `end_repair`, `request_history`,
#  `Purpose` and `Pending::History` — none of which survived `seam::link`
#  replacing that transport (see `seam/link.rs`'s own module doc, "What this
#  replaces", which names this exact list). There was no `tests/transport.rs`
#  left for it to drive either. Measured: every mutation in the retired script
#  came back `ANCHOR-MISSING` (0 matches) against this worktree's
#  `src/transport.rs`. The property it existed for — reconnect discrimination,
#  `Lagged` recovery, the outbound drain — now lives one layer down, in
#  `lib/shapewire/tests/resubscribe_identity.rs` (synthetic frames) and
#  `lib/shapewire/seam/probe/` (measured live, 2026-10-07); this crate's own
#  `tests/seam_conformance.rs` and `tests/seam_reference.rs` cover the seam
#  surface that replaced it.
#
#  Usage:  ./conformance_test.sh
#  Exit:   0 everything proven; 1 otherwise.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
# ── BUILD INTO A CONFORMANCE-ONLY TARGET DIRECTORY ───────────────────────────
# The operator's LIVE face executes `$REPO_ROOT/target/debug/tui`, and every
# cargo invocation below rebuilds whatever `CARGO_TARGET_DIR` names. Sourcing
# this exports a target directory that is NOT the live one and REFUSES to run
# if the resolved one ever is. The whole argument is in that file.
. "$CRATE_DIR/conformance_target.sh"

# ── AND MUTATE A COPY, NEVER THE WORKING TREE ────────────────────────────────
# This suite MUTATES source to prove its checks can still detect the defects
# they exist for. A trap restores the tree on every exit path it can catch —
# and `SIGKILL` is not one of them. On 2026-09-22 a worker was killed between a
# `mutate` and its `restore` and left a live mutation in `src/transport.rs`,
# which then stood as the tree the next worker would measure. Sourcing this
# re-points `CRATE_DIR`/`REPO_ROOT` at a cheap APFS clone of the workspace, so
# every mutation and every build below lands on the copy. The working tree is
# never written to, which is why the property survives being killed.
. "$CRATE_DIR/conformance_mirror.sh"

BIN="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/tui"
TERM_RS="$CRATE_DIR/src/term.rs"
MAIN_RS="$CRATE_DIR/src/main.rs"
BACKUP="$(mktemp -d)"
PROBE="$BACKUP/probe.py"
# **A SCRATCH HOME FOR THE FACE (R7.S5b).** The face resolves `~` once
# (`dirs::home_dir()`) and under it reads `~/.cauldron` — the project registry,
# which a registry act REWRITES — and writes `~/.tui/log` and `~/.tui/diag.log`
# beside his live face's. The probe used to inherit the real HOME. Kept apart
# from `$BACKUP`, which is deliberately LEFT on disk when a restore fails.
FACEHOME="$(mktemp -d)"
mkdir -p "$FACEHOME/.tui"
FAKE_PID=""
FAILURES=0

# **THE RESTORE MUST BE VERIFIED, NOT ATTEMPTED.** Measured this round, and it
# is the worst failure the harness has: the disk filled mid-run, `cp` failed,
# the trap ignored the exit status, `rm -rf "$BACKUP"` destroyed the pristine
# copy, and the crate was left carrying section 3e's mutation — a blanked
# `FRAME_BEGIN`/`FRAME_END` and an extra `term::CLAIMS` entry — with NOTHING
# said about it. The next thing to read that tree would have measured a mutant
# and called the answer truth. So each file is copied back, COMPARED, and the
# backup is deleted only if every comparison holds; if one does not, the run
# says so in the loudest terms it has and LEAVES THE BACKUP ON DISK.
cleanup() {
    local failed=0 src dest
    [ -n "$FAKE_PID" ] && kill "$FAKE_PID" 2>/dev/null
    rm -rf "$FACEHOME"
    for src in term.rs main.rs; do
        [ -f "$BACKUP/$src" ] || continue
        case "$src" in term.rs) dest="$TERM_RS" ;; main.rs) dest="$MAIN_RS" ;; esac
        cp "$BACKUP/$src" "$dest" 2>/dev/null
        cmp -s "$BACKUP/$src" "$dest" && continue
        failed=1
        printf '\n\033[31m*** RESTORE FAILED — %s IS STILL MUTATED\033[0m\n' "$dest"
        printf '    pristine copy kept at: %s\n' "$BACKUP/$src"
    done
    if [ "$failed" -eq 1 ]; then
        printf '\033[31m*** THE TREE IS LEFT MUTATED AND THE BACKUP IS NOT DELETED.\033[0m\n'
        printf '    Put those files back BEFORE anything else reads this crate: a\n'
        printf '    mutated tree measures as truth and reports as a green. A FULL DISK\n'
        printf '    is how this happens — cp fails and has nothing to write with.\n'
        return 1
    fi
    rm -rf "$BACKUP"
}
# The scratch target tree goes with it: bash's traps are last-write-wins, so a
# suite that installs its own EXIT trap would otherwise silently discard the
# one `conformance_target.sh` set and leave half a gigabyte behind.
trap 'cleanup; conformance_target_release' EXIT INT TERM
cp "$TERM_RS" "$BACKUP/term.rs"
cp "$MAIN_RS" "$BACKUP/main.rs"

# **REFUSE TO RUN OVER SOMEBODY ELSE'S MUTATION**, the way `mic_conformance_
# test.sh` §0 already does. This is the other half of the repair above: the
# verified restore stops a run from LEAVING a mutant, and this stops a run from
# starting on top of one and backing it up as if it were the real source — which
# is how a stranded mutation becomes permanent.
if grep -rn 'CONFORMANCE PROBE' "$CRATE_DIR/src" "$CRATE_DIR/../../libs/tui-render/src" 2>/dev/null; then
    echo
    echo "REFUSING TO RUN: a source above still carries a mutation marker."
    echo "A previous run was killed, or its restore failed, before the tree came"
    echo "back. Restore it first — everything below would measure the mutant and"
    echo "the backup taken a moment ago would make the mutation the new baseline."
    exit 3
fi

ok()    { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
bad()   { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
head_() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# **A HUNG PROBE IS A FAILURE OF THE SUITE, AND IT IS COUNTED AS ONE.** Same
# shape as `anchor_missing` below and for the same reason: a step that measured
# NOTHING must never reach a predicate, because every predicate here reads an
# absence as an answer and a timed-out probe's output is nothing but absences.
# The forced-failure sections are where that bites hardest — `drag_claimed ""`
# is false, and "false" is the branch those sections call a triumph.
probe_timed_out() { # <probe-output> <section>
    case "$1" in
        *PROBE-TIMEOUT*)
            bad "$2: PROBE-TIMEOUT — the face never exited, the wait was bounded and the child was killed. This check measured NOTHING (the suite did not hang; it refused to)."
            printf '%s\n' "$1" | sed -n 's/^PROBE-TIMEOUT /        timeout: /p'
            return 0 ;;
    esac
    return 1
}

build() { (cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -3); }

# ── the probe ────────────────────────────────────────────────────────────────
# Runs the binary under a real pty at a fixed size, sends one keystroke, and
# prints three machine-readable lines plus the rendered screen:
#   MODES  <the DEC private modes, in the order they were written>
#   PANIC_AFTER_ALT <yes|no|none>   (did the panic text land on the PRIMARY
#                                    screen, i.e. after ?1049l)
#   SCREEN <the grid, one row per line>
cat > "$PROBE" <<'PY'
import os, pty, re, select, signal, sys, termios, fcntl, struct, time
COLS, ROWS, BIN = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
EXTRA = sys.argv[4:]
# The scratch home and the fake daemon's port (R7.S5b), handed over by
# `probe()`. A KeyError, not a fallback: a probe without them must not start a
# face at all.
FACE_HOME = os.environ["TUI_PROBE_HOME"]
FACE_PORT = os.environ["TUI_PROBE_PORT"]
pid, fd = pty.fork()
if pid == 0:
    env = dict(os.environ)
    env["TERM"] = "xterm-256color"
    # A port nothing listens on: this suite is about the TERMINAL, and it must
    # not reach the operator's daemon. The face renders "connecting"/"retry in
    # …" and the bands paint exactly the same. These are the variables the face
    # READS (R7.S5): it used to set only `TUI_THREADS_URL`, which nothing reads,
    # so the seam fell back to 3653, production threads. The set is pinned
    # against `apps/tui/src` by `tests/drive_isolation.rs`.
    # R7.S5b: the PORT is the crate's own fake daemon (see `probe()`), not 1 —
    # on port 1 the root is "threads is down" and has no bands, so section 1's
    # body check could only fail.
    env["TUI_THREADS_CHASSIS_PORT"] = FACE_PORT
    env["TUI_THREADS_PORT"] = FACE_PORT
    env["AUI_V2_URL"] = "ws://127.0.0.1:1/"
    env["CAULDRON_BIN"] = "/usr/bin/false"
    env["CAULDRON_ISOLATED"] = "1"
    # REL-6a6: an isolated face dials only the scratch ports it is GIVEN.
    # REL-6a8: with the pid holding it — the fake is not the face's child, so
    # the face accepts it only as the listener this harness names.
    env["CAULDRON_ISOLATED_PORTS"] = f"127.0.0.1:{FACE_PORT}@{os.environ['TUI_FAKE_PID']}" if os.environ.get("TUI_FAKE_PID") else FACE_PORT
    env["CAULDRON_OPENER"] = "none"; env["CAULDRON_TYPE_ONE"] = "off"
    # NOT HIS HOME (R7.S5b): `~/.cauldron` and `~/.tui` are his live face's.
    env["HOME"] = FACE_HOME
    # NOT HIS STORE (REL-1.S1c): the --cauldron face's root, under the scratch home.
    env["CAULDRON_HOME"] = os.path.join(FACE_HOME, "chome")
    # NOT HIS TMUX (R7.S5b): run inside his tmux, the face's terminal
    # negotiation (`term::tmux_option`, `passthrough_allowed`) would query his
    # live tmux server. `pty_drive.py` and `composer_frames.sh` already pop it.
    env.pop("TMUX", None)
    os.execve(BIN, [BIN] + EXTRA, env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
# **NEVER STOP READING THE PTY.** This loop used to `time.sleep(0.4)` before
# sending the quit, and that sleep was not a pause — it was a FREEZE. A pty's
# kernel buffer is small; a face that keeps drawing fills it and then BLOCKS in
# `write`, so a parent that is not reading is a parent that has stopped the
# child. Measured: every run returned byte-identical output (5488 bytes) with
# exactly ONE `?2026h` in it, because the second frame was never drawn rather
# than never read. That made every per-FRAME property unobservable — the whole
# stream looked, to any check, exactly like a mode claimed once for the session.
# The wait is therefore armed as a DEADLINE and the loop keeps draining.
# CAULDRON (R7.S1b): 30 s, was 6. MEASURED 2026-10-14 on this machine: the
# FIRST exec of a freshly linked binary took 5.57 s before it ran a line (the
# second exec of the same file, 0.00 s) — macOS assesses a new executable. Every
# mutation in section 3 probes a freshly linked binary, so at 6 s the face had
# not drawn 200 bytes by the deadline, the `q` was never sent, and five of them
# came back PROBE-TIMEOUT. This is a DEADLINE, not a delay: the loop sends `q`
# 1.2 s after the first 200 bytes and re-arms to 2 s, exactly as before.
buf, sent, deadline, arm = b"", False, time.time() + 30.0, None
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.05)
    if r:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
    if not sent and len(buf) > 200:
        if arm is None:
            arm = time.time() + 1.2
        elif time.time() >= arm:
            try:
                os.write(fd, b"q")
            except OSError:
                pass
            sent, deadline = True, time.time() + 2.0
# **BOUND THE WAIT. A HANG MUST BECOME A RED, NOT A PAUSE.**
#
# This was `os.waitpid(pid, 0)` — no timeout — and it is the same family as the
# `time.sleep` above: a probe that BLOCKS instead of bounding its wait. A face
# that does not exit on `q` left this call parked with no deadline and the
# whole suite stopped, silently, forever. Reproduced against a child that
# ignores the quit and every catchable signal: the probe printed NOTHING and
# was still blocked 25 seconds later.
#
# Why it matters more than a slow suite. A run that hangs is a run somebody
# eventually kills — and section 3 kills it IN THE MIDDLE OF A MUTATION, with
# `term.rs` or `main.rs` still carrying a deliberately broken edit. The next
# thing to read that tree measures the mutant and calls the result truth. So
# the wait is bounded, the child is killed rather than waited on, and the
# timeout is REPORTED on a line the suite turns into a FAILURE — the process
# then exits NORMALLY, through the shell, so every `restore_sources` and the
# EXIT trap still run and the tree comes back.
# **KEEP DRAINING WHILE YOU WAIT.** The same lesson as the loop above, one
# scope down: a child blocked in `write` to a pty nobody is reading is a child
# that cannot reach its own exit, so a waiter that stops reading can MANUFACTURE
# the hang it is there to detect. `_reap` therefore drains as it waits.
def _reap(deadline, drain=True):
    while True:
        try:
            done, _ = os.waitpid(pid, os.WNOHANG)
        except OSError:
            return True          # already reaped: gone is gone, not a failure
        if done == pid:
            return True
        if time.time() >= deadline:
            return False
        if drain:
            try:
                r, _, _ = select.select([fd], [], [], 0.02)
                if r and not os.read(fd, 65536):
                    drain = False
            except OSError:
                drain = False
        else:
            time.sleep(0.02)

# The grace is DELIBERATELY LONG. Its job is to separate "does not exit" from
# "is slow to exit", and only the first is a defect — a face that takes four
# seconds to unwind a retrying websocket is behaving correctly and must not be
# reported as hung. Twelve seconds costs a suite nothing on the runs where the
# face exits (it returns the instant the child is reaped) and costs twelve
# seconds on the runs where it does not, which used to cost forever.
def _signal(sig):                # never let a kill raise: the child may be gone
    try:
        os.kill(pid, sig)
        return True
    except OSError:
        return False

timed_out = None
if not _reap(time.time() + 12.0):
    timed_out = "the probed child (pid %d) did not exit within 12s of the quit" % pid
    _signal(signal.SIGTERM)
    if _reap(time.time() + 2.0):
        timed_out += "; SIGTERM reaped it"
    else:
        # Closing the master hangs the slave up, which is what frees a child
        # wedged on terminal I/O. SIGKILL alone does not always do it.
        try:
            os.close(fd)
        except OSError:
            pass
        fd = -1
        _signal(signal.SIGKILL)
        if _reap(time.time() + 3.0, drain=False):
            timed_out += "; hangup + SIGKILL reaped it"
        else:
            timed_out += "; SIGKILL did not reap it — pid %d may be ORPHANED" % pid
if fd >= 0:
    try:
        os.close(fd)
    except OSError:
        pass

# Printed FIRST, and before MODES, so a reader sees the diagnosis before the
# half-measured stream it explains. The lines below still print: a timed-out
# probe's bytes are real bytes and are worth seeing, they are just incomplete,
# and no predicate below may be allowed to read them as a result.
if timed_out:
    print("PROBE-TIMEOUT " + timed_out)

modes = [f"?{m.decode()}{a.decode()}" for m, a in re.findall(rb"\x1b\[\?(\d+)([hl])", buf)]
print("MODES " + " ".join(modes))

leave = buf.find(b"\x1b[?1049l")
panic = buf.find(b"panicked at")
if panic < 0:
    print("PANIC_AFTER_ALT none")
else:
    print("PANIC_AFTER_ALT " + ("yes" if leave >= 0 and panic > leave else "no"))

grid = [[" "] * COLS for _ in range(ROWS)]
row = col = i = 0
while i < len(buf):
    b = buf[i]
    if b == 0x1B:
        m = re.match(rb"\x1b\[([0-9;?]*)([A-Za-z])", buf[i:])
        if m:
            params, final = m.group(1), m.group(2)
            if final == b"H":
                p = params.split(b";")
                row = (int(p[0]) - 1) if p and p[0] else 0
                col = (int(p[1]) - 1) if len(p) > 1 and p[1] else 0
            elif final == b"J" and params in (b"2", b""):
                grid = [[" "] * COLS for _ in range(ROWS)]
            i += m.end(); continue
        i += 1; continue
    if b == 0x0D: col = 0; i += 1; continue
    if b == 0x0A: row += 1; col = 0; i += 1; continue
    ln = 4 if b >= 0xF0 else 3 if b >= 0xE0 else 2 if b >= 0xC0 else 1
    try:
        ch = buf[i:i+ln].decode("utf-8")
    except UnicodeDecodeError:
        i += 1; continue
    if 0 <= row < ROWS and 0 <= col < COLS:
        grid[row][col] = ch
    col += 2 if any(0x1100 <= ord(c) <= 0x115F or 0x2E80 <= ord(c) <= 0xA4CF or
                    0xAC00 <= ord(c) <= 0xD7A3 or 0xFF00 <= ord(c) <= 0xFF60 for c in ch) else 1
    i += ln
for r in grid:
    print("SCREEN " + "".join(r).rstrip())
PY

probe() { TUI_PROBE_HOME="$FACEHOME" TUI_PROBE_PORT="$FAKE_PORT" TUI_FAKE_PID="$FAKE_PID" python3 "$PROBE" "${1:-100}" "${2:-24}" "$BIN" ${3:+"$3"}; }

# ── THE FAKE DAEMON THE PROBE TALKS TO (R7.S5b) ──────────────────────────────
# R7.S5 pointed the probe at port 1, which is right for isolation and wrong for
# section 1: a face with no daemon paints "threads is down" and no band, so
# `painted_body` went red on every run (measured: `ANCHOR-MISSING body`). Until
# R7.S5 the bands were there only because the probe was talking to PRODUCTION
# threads. `examples/fake_daemon` binds 127.0.0.1 and answers from memory; it is
# not his daemon and cannot reach one. Started once; killed by `cleanup`.
FAKE_PORT="${TUI_FAKE_PORT:-18795}"
case "$FAKE_PORT" in
    3651|3653|3753|9010) echo "REFUSING: TUI_FAKE_PORT=$FAKE_PORT is a LIVE port"; exit 3 ;;
esac
port_listening() { python3 -c 'import socket,sys
s = socket.socket(); s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1", int(sys.argv[1]))) == 0 else 1)' "$1"; }
if port_listening "$FAKE_PORT"; then
    echo "ABORTING: 127.0.0.1:$FAKE_PORT is already taken; set TUI_FAKE_PORT to a free port"
    exit 1
fi
(cd "$REPO_ROOT" && cargo build -p tui --example fake_daemon >/dev/null 2>&1)
"${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/examples/fake_daemon" "$FAKE_PORT" >"$FACEHOME/fake.log" 2>&1 &
FAKE_PID=$!
BOUND=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    kill -0 "$FAKE_PID" 2>/dev/null || break
    if port_listening "$FAKE_PORT"; then BOUND=1; break; fi
    sleep 0.5
done
[ "$BOUND" -eq 1 ] || { echo "ABORTING: the fake daemon never listened on 127.0.0.1:$FAKE_PORT"; exit 1; }

# ── the three properties, as functions, so a mutant can be held to the same
#    bar as the real thing ────────────────────────────────────────────────────
modes_of()      { printf '%s' "$1" | sed -n 's/^MODES //p'; }
claims_exact()  { # entry set is exactly CLAIMS, in that order
    local m; m="$(modes_of "$1")"
    # `?2004h` joined the set with the COMPOSER: without bracketed paste a paste
    # arrives as a burst of keystrokes whose trailing carriage return cannot be
    # told from `enter`, which SENDS. It is the last entry claim; everything
    # after it in the stream is the per-frame `?2026` pair and the resets.
    case "$m" in "?1049h ?25l ?1002h ?1006h ?2004h "*) return 0 ;; *) return 1 ;; esac
}
# **SYNCHRONIZED OUTPUT IS USED, AND IS NOT AN ENTRY CLAIM.** `?2026` is a
# begin/end pair written around ONE frame, so it must appear in the stream and
# must NOT be a mode held for the session — a held mode would make `CLAIMS`'s
# promise ("these are set right now") false between frames.
#
# **HOW THIS CHECK USED TO BE WRITTEN, AND WHY IT COULD NOT HAVE WORKED.** It
# asked whether the stream STARTED WITH the five entry claims followed by
# `?2026h`, and treated that as "2026 was taken as a session mode". It is not:
# the first frame is painted immediately after entry, so a correct per-frame
# pair produces EXACTLY that prefix. Measured on the shipped binary:
#
#   ?1049h ?25l ?1002h ?1006h ?2004h ?2026h ?25l ?2026l …
#
# The two shapes are therefore INDISTINGUISHABLE BY POSITION, and they are also
# indistinguishable by count in a one-frame run. (The old code hid this: both
# branches of that `case` returned 0, so the half never ran. Making the match
# branch `return 1` — the obvious repair — would have turned it into a FALSE
# RED against correct shipped code.)
#
# What DOES discriminate is the ARITY. A per-frame pair writes one `?2026h` per
# frame; a session claim writes exactly one for the whole run however many
# frames are drawn. So the property is checked as: the 2026 events, in order,
# must read `h l h l …` with MORE THAN ONE `h`, allowing the single trailing
# `l` that `term::restore` emits unconditionally for the panic case. This is
# only observable because the probe above no longer freezes the child.
sync_events() { printf '%s\n' "$(modes_of "$1")" | tr ' ' '\n' | sed -n 's/^?2026\([hl]\)$/\1/p' | tr -d '\n'; }
sync_per_frame() {
    local seq begins
    seq="$(sync_events "$1")"
    # (1) used at all
    case "$seq" in *h*) ;; *) return 1 ;; esac
    # (2) MORE THAN ONE begin — the discriminator against a session-held mode
    begins="$(printf '%s' "$seq" | tr -cd h | wc -c | tr -d ' ')"
    [ "$begins" -ge 2 ] || return 1
    # (3) strict alternation, opening with a begin, one trailing reset allowed
    case "$seq" in h*) ;; *) return 1 ;; esac
    case "${seq%l}" in *hh*|*ll*) return 1 ;; esac
    return 0
}
# **THE CHECK THAT CHANGED SIDES, AND IT IS THE WHOLE OF `IS-21`.**
#
# It used to assert that ?1002 was NEVER requested. `0036` measured why that
# was the defect rather than the guard: ?1000h alone reports press and release
# with NO motion, so the app could not see a drag, and tmux — whose drag
# binding branches on `mouse_any_flag`, which ?1000h also sets — forwarded the
# drag to us and did no selection of its own. Neither side selected. So ?1002h
# is now REQUIRED and ?1003h (idle motion with no button held, the repaint
# storm `0018` measured) is still forbidden, as is ?1015 (urxvt coordinates,
# which nothing here speaks).
drag_claimed()  { case "$(modes_of "$1")" in *\?1002h*) return 0 ;; *) return 1 ;; esac; }
no_idle_motion() { case "$(modes_of "$1")" in *\?1003*|*\?1015*) return 1 ;; *) return 0 ;; esac; }
mouse_first()   { # every mouse reset precedes the alternate-screen reset
    local m; m="$(modes_of "$1")"
    local before="${m%%\?1049l*}"
    [ "$before" != "$m" ] || return 1
    case "$before" in *\?1006l*) ;; *) return 1 ;; esac
    case "$before" in *\?1002l*) ;; *) return 1 ;; esac
    return 0
}
cursor_back()   { case "$(modes_of "$1")" in *\?25h*) return 0 ;; *) return 1 ;; esac; }

# ── 1. the real binary, clean quit ───────────────────────────────────────────
head_ "1. the real binary — entry claims, and a clean quit"
build >/dev/null
OUT_CLEAN="$(probe 100 24)"
# Blanked on a timeout ON PURPOSE. These are DIRECT assertions, so an empty
# stream reddens every one of them, which is the honest reading: nothing was
# measured. (The forced-failure sections cannot do this — see below.)
probe_timed_out "$OUT_CLEAN" "1 · the real binary, clean quit" && OUT_CLEAN=""
printf '     modes: %s\n' "$(modes_of "$OUT_CLEAN")"
claims_exact "$OUT_CLEAN" && ok "the entry set is exactly term::CLAIMS, in order" \
                          || bad "the entry set is not term::CLAIMS"
drag_claimed "$OUT_CLEAN" && ok "?1002h IS claimed — the face can see a drag at all (IS-21)" \
                          || bad "?1002h is not claimed: the face is back in the ?1000h dead zone"
no_idle_motion "$OUT_CLEAN" && ok "no ?1003/?1015 — the any-motion repaint storm stays gone" \
                            || bad "any-motion tracking was requested"
sync_per_frame "$OUT_CLEAN" && ok "?2026 is written per FRAME and is not an entry claim" \
                           || bad "synchronized output is missing, or was taken as a session mode"
mouse_first "$OUT_CLEAN" && ok "clean exit: the mouse is released BEFORE the alternate screen is left" \
                         || bad "clean exit: the alternate screen was left with the mouse still captured"
cursor_back "$OUT_CLEAN" && ok "clean exit: the cursor comes back" \
                         || bad "clean exit: the cursor was left hidden"
# WHAT THIS CHECK USED TO SAY, and why it was wrong. It looked for the word
# `composer` on the painted grid and called that "the five-band frame". That
# was true in build step 1, when a thread view was what the face opened on. In
# build step 3 the startup view became `Mode::List` — the fleet — which
# correctly has NO composer band, and this check has been RED ever since
# without anything acting on it. A conformance check that went stale under a
# later round is worse than no check: it reports a defect that is not there and
# trains a reader to skip the section.
#
# So it now asserts what is actually true of the frame the face opens on: the
# HEADER band on row 0 and the FOOTER band on the last row, with body between
# them. Both are present in every mode, which is what makes them the right
# thing to key a paint check on.
#
# UPDATED AGAIN, and for a reason worth recording beside the last one: the face
# no longer opens on `threads`. Its root is PROJECTS, following the AUI, so the
# header band's first word changed and this predicate would have gone stale a
# second time inside one round. It is keyed on the two bands rather than on
# either word — `· ` is the header's separator grammar and `q quit` is the
# footer's last hint — so the next change of view does not break it.
# UPDATED A THIRD TIME, by `0041`: the footer's right-hand end used to read
# `mouse: face`, the state of the `M` toggle. `M` is deleted — the face selects
# for itself now, so there is nothing to hand back — and the indicator went
# with it. This predicate never keyed on that word, which is why it did not go
# stale again; it is noted here so the next reader knows the omission is
# deliberate.
# **RE-KEYED IN `0072`, AND THE OLD KEY WENT STALE EXACTLY AS PREDICTED.**
#
# It read `^SCREEN projects · `, which was the root header's TITLE. The operator
# cut that header to three things — "All I want to see is the # unread …, and
# connected" — so the word `projects` left row 0 and this check went red. That
# red was CORRECT: the anchor did its job. It is re-keyed rather than deleted,
# because a paint check that cannot detect a blank screen is worse than none.
#
# **IT IS KEYED ON STRUCTURE NOW, NOT ON WORDING**, which is why it went stale
# twice. Three bands, three predicates, each naming itself when it fails so the
# next reader gets the right diagnosis instead of "the frame did not paint":
#
#   HEADER — the LINK's own word. It is on row 0 in every state the face can be
#            in (connected / connecting / disconnected), it is one of the three
#            facts he explicitly kept, and it is the last thing a header cut
#            would remove.
#   BODY   — a BAND HEADER with its count. Stronger than the old check ever
#            was: the old one proved CHROME drew, this proves the root's
#            SURFACE drew. `[▸ ]` because a band header is a cursor stop since
#            `DEC-46` and may be wearing the marker.
#   FOOTER — unchanged.
painted_header() { printf '%s' "$1" | grep -qE "^SCREEN.*(connected|connecting)"; }
painted_body()   { printf '%s' "$1" | grep -qE "^SCREEN [▸ ] *(pinned|recent|projects) · [0-9]"; }
painted_footer() { printf '%s' "$1" | grep -q "^SCREEN.*q quit"; }
painted() { # $1 = probe output
    painted_header "$1" || return 1
    painted_body   "$1" || return 2
    painted_footer "$1" || return 3
    return 0
}
painted "$OUT_CLEAN"
case $? in
  0) ok "the frame painted: header, a band of the root surface, and the footer" ;;
  1) bad "ANCHOR-MISSING header — no link word on row 0 (re-key painted_header)" ;;
  2) bad "ANCHOR-MISSING body — no band header with a count (re-key painted_body)" ;;
  3) bad "ANCHOR-MISSING footer — no 'q quit' (re-key painted_footer)" ;;
esac

# FORCED: below `geom::MIN_WIDTH` x `MIN_HEIGHT` (40x8) the face draws ONE
# sentence and no bands at all, deliberately — four bands squeezed into six
# rows would have to lie about at least one of their sizes. So the same
# predicate must go RED there, or it is not reading the bands, it is reading
# the fact that the program ran.
OUT_TINY="$(probe 30 6)"
if probe_timed_out "$OUT_TINY" "1 · the too-small frame"; then
    : # a blank screen from a killed child is not the same thing as no bands
elif painted "$OUT_TINY"; then
    bad "the paint check passes a frame with no bands on it — it proves nothing"
else
    ok "the same check goes red on the too-small frame (forced failure observed)"
    printf '     too-small grid says: %s\n' "$(printf '%s' "$OUT_TINY" | sed -n 's/^SCREEN //p' | head -1)"
fi

# ── 2. the real binary, forced panic ─────────────────────────────────────────
head_ "2. the real binary — a forced panic"
OUT_PANIC="$(probe 100 24 --panic-now)"
probe_timed_out "$OUT_PANIC" "2 · the real binary, forced panic" && OUT_PANIC=""
printf '     modes: %s\n' "$(modes_of "$OUT_PANIC")"
mouse_first "$OUT_PANIC" && ok "panic: the mouse is released BEFORE the alternate screen is left" \
                         || bad "panic: the alternate screen was left with the mouse still captured"
cursor_back "$OUT_PANIC" && ok "panic: the cursor comes back" \
                         || bad "panic: the cursor was left hidden"
case "$OUT_PANIC" in
    *"PANIC_AFTER_ALT yes"*) ok "panic: the message lands on the PRIMARY screen, not in a discarded buffer" ;;
    *) bad "panic: the message was printed before the alternate screen was left" ;;
esac

# ── 3. FORCED FAILURES — each assertion, broken on purpose ───────────────────
# **THE MUTATION'S EXIT STATUS IS THE WHOLE POINT OF THE MUTATION.** This
# function used to discard it. When an anchor MOVED, the `assert` fired, the
# edit did not happen, and the function went on to build and probe the
# UNMUTATED binary — which of course still behaves correctly, so the caller
# reported "the check proves nothing". That is the wrong diagnosis wearing the
# shape of a pass: the check was fine, the ANCHOR was stale, and the two need
# completely different repairs. `0065` splits them: ANCHOR MOVED is now its own
# loud outcome and never reaches a predicate.
mutate_and_probe() { # <python-edit-script> <label> [extra-arg]
    local label="${2:-unlabelled}" perr rc
    perr="$(python3 - "$TERM_RS" "$MAIN_RS" 2>&1 >/dev/null <<PY
import pathlib, sys
term = pathlib.Path(sys.argv[1]); main = pathlib.Path(sys.argv[2])
$1
PY
)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'ANCHOR-MISSING %s\n' "$label"
        printf 'ANCHOR-DETAIL %s\n' "$(printf '%s' "$perr" | tr '\n' ' ' | tail -c 200)"
        return 3
    fi
    build >/dev/null
    probe 100 24 ${3:+"$3"}
}
# Every forced-failure section below asks this FIRST. A stale anchor is a
# FAILURE of the suite and is counted as one; it is never silently a pass and
# never a complaint about the code.
anchor_missing() { # <probe-output> <section>
    case "$1" in
        *ANCHOR-MISSING*)
            bad "$2: ANCHOR-MISSING — the mutation target MOVED. This check is proving NOTHING until its anchor is repaired in conformance_test.sh (the code did not fail; the mutation never applied)."
            printf '%s\n' "$1" | sed -n 's/^ANCHOR-MISSING /        anchor: /p;s/^ANCHOR-DETAIL /        said:   /p'
            return 0 ;;
    esac
    return 1
}
restore_sources() { cp "$BACKUP/term.rs" "$TERM_RS"; cp "$BACKUP/main.rs" "$MAIN_RS"; }

head_ "3a. forced failure: re-introduce ?1003h (crossterm's EnableMouseCapture set)"
# chr(92) rather than a backslash literal: this edit travels through a shell
# single-quote, an unquoted heredoc and a Python string, and a backslash does
# not survive all three the same way twice.
ESC='chr(92) + "x1b"'
OUT="$(mutate_and_probe 's = term.read_text()
n = '"$ESC"' + "[?1002h"
assert n in s, "the drag claim moved"
term.write_text(s.replace(n, n + '"$ESC"' + "[?1003h", 1))' idle-motion)"
if probe_timed_out "$OUT" "3a" || anchor_missing "$OUT" "3a"; then
    :
elif no_idle_motion "$OUT"; then
    bad "a smuggled ?1003h was NOT seen — the any-motion check proves nothing"
else
    ok "a smuggled ?1003h IS caught (forced failure observed)"
    printf '        modes: %s\n' "$(modes_of "$OUT")"
fi
restore_sources

# 3a-bis — THE OTHER DIRECTION, and it is the one that matters now. A regression
# to ?1000h is exactly the state `IS-21` describes, and it is the state this
# binary shipped in until `0041`. The check has to bite on it or the whole fix
# is unguarded.
head_ "3a-bis. forced failure: regress the claim to ?1000h (the IS-21 dead zone)"
OUT="$(mutate_and_probe 's = term.read_text()
n = '"$ESC"' + "[?1002h"
assert n in s, "the drag claim moved"
term.write_text(s.replace(n, '"$ESC"' + "[?1000h", 1))' drag-claim)"
if probe_timed_out "$OUT" "3a-bis" || anchor_missing "$OUT" "3a-bis"; then
    :
elif drag_claimed "$OUT"; then
    bad "a regression to ?1000h was NOT seen — the drag check proves nothing"
else
    ok "a regression to ?1000h IS caught (forced failure observed)"
    printf '        modes: %s\n' "$(modes_of "$OUT")"
fi
restore_sources

head_ "3b. forced failure: leave the alternate screen BEFORE releasing the mouse"
OUT="$(mutate_and_probe 's = term.read_text()
n = "    for claim in CLAIMS.iter().rev() {"
assert n in s, "restore() moved"
term.write_text(s.replace(n, "    for claim in CLAIMS.iter() {", 1))' restore-order)"
if probe_timed_out "$OUT" "3b" || anchor_missing "$OUT" "3b"; then
    :
elif mouse_first "$OUT"; then
    bad "a reversed restore order was NOT caught — the ordering check proves nothing"
else
    ok "a reversed restore order IS caught (forced failure observed)"
    printf '        modes: %s\n' "$(modes_of "$OUT")"
fi
restore_sources

head_ "3c. forced failure: no panic hook at all"
OUT="$(mutate_and_probe 's = main.read_text()
n = "    term::install_panic_hook();"
assert n in s, "the hook install moved"
main.write_text(s.replace(n, "    // CONFORMANCE PROBE: hook removed", 1))' panic-hook --panic-now)"
if probe_timed_out "$OUT" "3c" || anchor_missing "$OUT" "3c"; then
    :
elif mouse_first "$OUT" && cursor_back "$OUT"; then
    bad "a missing panic hook was NOT caught — the panic check proves nothing"
else
    ok "a missing panic hook IS caught: the terminal is left taken (forced failure observed)"
    printf '        modes: %s\n' "$(modes_of "$OUT")"
fi
restore_sources


head_ "3d. forced failure: drop a claim out of the entry set (claims_exact)"
# `claims_exact` had never been watched bite. Blanking ONE claim's set sequence
# is the smallest thing that makes the entry set wrong while leaving the face
# working, which is exactly the regression the check exists for.
OUT="$(mutate_and_probe 's = term.read_text()
n = chr(92) + "x1b[?2004h"
assert n in s, "the bracketed-paste claim moved"
term.write_text(s.replace(n, "", 1))' entry-set)"
if probe_timed_out "$OUT" "3d" || anchor_missing "$OUT" "3d"; then
    :
elif claims_exact "$OUT"; then
    bad "a claim dropped out of the entry set was NOT seen — claims_exact proves nothing"
else
    ok "a dropped entry claim IS caught (forced failure observed)"
    printf '        modes: %s\n' "$(modes_of "$OUT")"
fi
restore_sources

head_ "3e. forced failure: hold ?2026 for the SESSION instead of per frame"
# THE HAZARD `sync_per_frame` IS NAMED FOR, and until `0065` it could not fire:
# both branches of its second `case` returned 0. The mutation is the one
# `term::CLAIMS`'s own doc comment warns about — carry `?2026` in the table, so
# it is taken once at entry and handed back once at exit, and stop writing the
# per-frame pair. The face still works; the terminal is simply held in
# synchronized output for the whole session and `CLAIMS`'s promise is false
# between frames.
OUT="$(mutate_and_probe 's = term.read_text()
q = chr(34)
h = chr(92) + "x1b[?2026h"
l = chr(92) + "x1b[?2026l"
a = "pub const FRAME_BEGIN: &str = " + q + h + q + ";"
b = "pub const FRAME_END: &str = " + q + l + q + ";"
assert a in s, "FRAME_BEGIN moved"
assert b in s, "FRAME_END moved"
s = s.replace(a, "pub const FRAME_BEGIN: &str = " + q + q + ";", 1)
s = s.replace(b, "pub const FRAME_END: &str = " + q + q + ";", 1)
tail = "        release_key: Some(" + q + "^C" + q + ")," + chr(10) + "    }," + chr(10) + "];"
assert tail in s, "the end of CLAIMS moved"
claim = "    Claim { set: " + q + h + q + ", reset: " + q + l + q + ", what: " + q + "CONFORMANCE PROBE" + q + ", release_key: Some(" + q + "q" + q + ") },"
s = s.replace(tail, "        release_key: Some(" + q + "^C" + q + ")," + chr(10) + "    }," + chr(10) + claim + chr(10) + "];", 1)
term.write_text(s)' sync-per-frame)"
if probe_timed_out "$OUT" "3e" || anchor_missing "$OUT" "3e"; then
    :
elif sync_per_frame "$OUT"; then
    bad "?2026 held for the whole session was NOT seen — sync_per_frame proves nothing"
else
    ok "a session-held ?2026 IS caught (forced failure observed)"
    printf '        modes: %s\n' "$(modes_of "$OUT")"
    printf '        2026 events: %s  (a per-frame pair reads h l h l …)\n' "$(sync_events "$OUT")"
fi
restore_sources

head_ "3f. forced failure: print the panic BEFORE giving the screen back"
# §2's position check had never been forced either. `install_panic_hook` calls
# `restore()` and THEN the default hook; swapping the two lines leaves the face
# on the alternate screen while the backtrace is written into it, and the
# buffer is discarded a moment later with his crash report inside it.
OUT="$(mutate_and_probe 's = term.read_text()
n = "        restore();" + chr(10) + "        default(info);"
assert n in s, "the panic hook body moved"
term.write_text(s.replace(n, "        default(info);" + chr(10) + "        restore();", 1))' panic-position --panic-now)"
if probe_timed_out "$OUT" "3f" || anchor_missing "$OUT" "3f"; then
    :
else
    case "$OUT" in
        *"PANIC_AFTER_ALT yes"*)
            bad "the panic printed before the restore was NOT seen — the position check proves nothing" ;;
        *)
            ok "a panic printed into the discarded buffer IS caught (forced failure observed)"
            printf '        %s\n' "$(printf '%s' "$OUT" | sed -n 's/^PANIC_AFTER_ALT /PANIC_AFTER_ALT /p')" ;;
    esac
fi
restore_sources

# ── 4. the repaint law is enumerable in ONE grep ─────────────────────────────
head_ "4. the repaint law's set-points are enumerable in one grep"
# Both checks below are properties of the SOURCE TEXT, so both are written as
# functions OVER A FILE and both are forced against a mutated COPY. A copy and
# not the tree, deliberately: `app.rs` is NOT in this script's backup set, and a
# check that has to dirty a file it cannot restore is a worse check than one
# that reads a copy. The mutant text is never compiled and never needs to be.
setpoint_lines()  { grep -n 'force_repaint(' "$1" | grep -v 'fn force_repaint' | grep -v '///'; }
setpoint_count()  { setpoint_lines "$1" | grep -c 'force_repaint('; }
flag_assignments(){ grep -c 'needs_full_repaint = true' "$1"; }

# **THE BAR, AND THE RULING BEHIND IT (`0065`).** This was `>= 3` against an
# actual of 6 — loose enough that HALF the repaint law could be deleted and the
# check would still be green, and only two of the six set-points are proven
# individually by name anywhere (chat §2 covers the clipboard write and the
# relink; resize, `^L`, startup and the keyboard-query answer are covered by
# nothing else at all). So the loose bar was the only guard against deletion,
# and it did not guard.
#
# It is raised to a FLOOR AT THE ACTUAL, and deliberately not to an equality.
# The failure this law fears is a set-point going MISSING; a set-point being
# ADDED is normal, wanted work (`term.rs` names suspend/resume as a future one),
# and an equality would go red on it and train the next reader to edit the bar,
# which is how a check becomes a rubber stamp. The two anchors in this crate
# that DO assert an exact count are exact at ONE, where a second occurrence is
# itself the defect — that is an invariant, where an equality here would only
# record today's number. Raise this line when you add a set-point; never lower
# it, and never lower it to make a red go away.
SETPOINT_FLOOR=6
COUNT="$(setpoint_count "$CRATE_DIR/src/app.rs")"
setpoint_lines "$CRATE_DIR/src/app.rs" | sed 's/^/        /'
if [ "$COUNT" -ge "$SETPOINT_FLOOR" ]; then
    ok "$COUNT set-points (floor $SETPOINT_FLOOR), all reachable from one grep for force_repaint("
else
    bad "only $COUNT set-points found, floor is $SETPOINT_FLOOR — a set-point was DELETED and the repaint law has no route back"
fi
if grep -q 'needs_full_repaint' "$CRATE_DIR/src/app.rs" && \
   [ "$(flag_assignments "$CRATE_DIR/src/app.rs")" -eq 1 ]; then
    ok "the flag is set in exactly ONE place, so the grep above cannot be incomplete"
else
    bad "needs_full_repaint is assigned outside force_repaint — the enumeration is not the set"
fi

# FORCED, and this is the mutation that shows what raising the bar bought:
# delete ONE set-point. Under the old `>= 3` this copy was GREEN.
MUT_APP="$BACKUP/app_one_setpoint_deleted.rs"
sed 's/Input::Resize => self.force_repaint("resize"),/\/\/ CONFORMANCE PROBE: set-point deleted/' \
    "$CRATE_DIR/src/app.rs" > "$MUT_APP"
MUT_COUNT="$(setpoint_count "$MUT_APP")"
if [ "$MUT_COUNT" -eq "$COUNT" ]; then
    bad "the deletion mutant is identical to the source — the anchor moved and 4's forced failure proves nothing"
elif [ "$MUT_COUNT" -ge "$SETPOINT_FLOOR" ]; then
    bad "a DELETED set-point ($COUNT -> $MUT_COUNT) is still above the floor — the bar proves nothing"
else
    ok "one deleted set-point IS caught: $COUNT -> $MUT_COUNT, below the floor of $SETPOINT_FLOOR (forced failure observed; the old >= 3 bar passed this)"
fi

# FORCED: a second raw assignment, which is the bypass the exact-one bar exists
# for and which no count of call sites could ever see.
MUT_FLAG="$BACKUP/app_second_assignment.rs"
{ cat "$CRATE_DIR/src/app.rs"; printf '        self.needs_full_repaint = true; // CONFORMANCE PROBE\n'; } > "$MUT_FLAG"
if [ "$(flag_assignments "$MUT_FLAG")" -eq 1 ]; then
    bad "a second raw assignment was NOT seen — the exact-one bar proves nothing"
else
    ok "a second raw needs_full_repaint assignment IS caught ($(flag_assignments "$MUT_FLAG") found, exactly 1 is legal) (forced failure observed)"
fi

# ── 5. the crate, green ──────────────────────────────────────────────────────
head_ "5. the restored crate, green"
build >/dev/null
if (cd "$REPO_ROOT" && cargo test -p tui 2>&1 | grep -q "test result: FAILED"); then
    bad "the restored crate does not pass its own tests"
else
    ok "cargo test -p tui is green"
fi

head_ "result"
if [ "$FAILURES" -eq 0 ]; then
    printf '  \033[32mterminal ownership proven on a real pty, each check forced to fail first\033[0m\n\n'
    exit 0
fi
printf '  \033[31m%d check(s) failed\033[0m\n\n' "$FAILURES"
exit 1
