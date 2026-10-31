#!/usr/bin/env python3
"""**THE PTY DRIVER — one terminal, exact bytes in, a rendered grid out.**

Extracted from `composer_frames.sh` when `pointer_frames.sh` needed the same
thing. It was a heredoc inside one script; two copies of a terminal emulator
is two emulators that can disagree about what the face drew, which is the one
disagreement no assertion in either suite could see.

Every caller runs the face under a REAL pty, writes a SCRIPT of byte chunks
into it with waits between them, and reads back three things: the DEC private
modes it requested, the `CSI >` requests it made, and the grid it painted.

Usage:  pty_drive.py COLS ROWS BIN HOME SCRIPT [URL]

`SCRIPT` is a python expression evaluating to a list of `(bytes, wait_seconds)`
— which is the whole point of testing at this layer, since a test has to be
able to say `\x1b[<64;10;5M` and mean exactly those bytes.

`URL` (default `ws://127.0.0.1:1/ws`, a port nothing listens on) names the
threads daemon the face may reach — a fake one on a scratch port, or nothing.
The face does not take a URL: its seam reads ONE port variable and builds
`ws://127.0.0.1:<port>/ws` itself, so only the PORT of `URL` is used, and a URL
whose host or path the face would not honour is refused rather than half-obeyed.

**ISOLATION (R7.S5, H179-1).** This used to set `TUI_THREADS_URL`, which
NOTHING in `apps/tui/src` reads; the seam reads `TUI_THREADS_CHASSIS_PORT`,
whose default 3653 is PRODUCTION threads. So every face driven by this could
reach the operator's daemon while its header said it could not. The face's
endpoint variables are now set explicitly (the `env[...] =` lines below — the
list is pinned against `apps/tui/src` by `tests/drive_isolation.rs`), and
`refuse()` checks the child's environment immediately before `execve`: a
missing or production-valued endpoint, or HOME naming his real home (R7.S5b),
stops the drive with exit 3 before a face exists.
"""
import os, pty, pwd, re, select, sys, termios, fcntl, struct, time
from urllib.parse import urlsplit

# Every variable in `apps/tui/src` that decides where the face connects or what
# it executes against a live system. `tests/drive_isolation.rs` keeps this in
# step with the source.
#   TUI_THREADS_CHASSIS_PORT  the seam's ONE socket (`seam::link::chassis_port`);
#                             unset or 0 or junk -> 3653, production threads
#   TUI_THREADS_PORT          the /health half (`seam::link::health_port`);
#                             `mirror::check_ports` refuses it set alone
#   AUI_V2_URL                the second socket (`aui::resolve_endpoint`);
#                             unset -> ws://127.0.0.1:9010/, the live ui-server
#   CAULDRON_BIN              what the restart button execs
#                             (`cauldron_env::cauldron_bin`): `cauldron --env
#                             <env> start threads`
#   CAULDRON_OPENER           what `o` execs in the `--cauldron` face
#                             (`cauldron::cli::Opener`); unset -> the system
#                             `open` on his desktop (IS-111); `none` execs nothing
#   CAULDRON_TYPE_ONE         the `--cauldron` seeding's real model call
#                             (`cauldron::seeding::real_call`); unset -> the
#                             `type-one` CLI, Haiku through his threads daemon or
#                             Jev through OpenRouter; `off` forces the local stand-in
#   CAULDRON_HOME             the `--cauldron` face's STORE ROOT
#                             (`env::Environment::resolve`, REL-1.S1c); set, it
#                             beats HOME, and the face WRITES under it (bucket,
#                             dumps, the repo registry). Set here to a scratch
#                             dir under the scratch HOME; refused unless it is
#                             inside that HOME and not inside his real home
ENDPOINT_VARS = ("TUI_THREADS_CHASSIS_PORT", "TUI_THREADS_PORT", "AUI_V2_URL", "CAULDRON_BIN", "CAULDRON_OPENER", "CAULDRON_TYPE_ONE", "CAULDRON_HOME")
# Live listeners on this machine: prod threads chassis, the retired legacy
# door, stage threads, the AUI's proto-v2 listener.
PROD_PORTS = {3651, 3653, 3753, 9010}


def threads_port(url):
    u = urlsplit(url)
    if u.scheme != "ws" or u.hostname != "127.0.0.1" or u.path != "/ws" or u.port is None:
        sys.exit(f"pty_drive: URL {url!r} is not ws://127.0.0.1:<port>/ws — the face "
                 "builds exactly that from one port, so any other host or path would be "
                 "silently ignored")
    return u.port


def refuse(env):
    """Why `env` may not be handed to a face, or None. Checked on the child's
    final environment, so it judges what the face will read, not what this file
    meant to set."""
    for k in ("TUI_THREADS_CHASSIS_PORT", "TUI_THREADS_PORT"):
        v = env.get(k, "").strip()
        if not v.isdigit() or not 0 < int(v) < 65536:
            return f"{k}={v!r} is unset or not a port; the seam would fall back to 3653 (PRODUCTION threads)"
        if int(v) in PROD_PORTS:
            return f"{k}={v} is a live port ({sorted(PROD_PORTS)})"
    aui = env.get("AUI_V2_URL", "").strip()
    try:
        aui_port = urlsplit(aui).port
    except ValueError:
        aui_port = None
    if not aui or aui_port is None or aui_port in PROD_PORTS:
        return f"AUI_V2_URL={aui!r} is unset, has no explicit port, or is a live port; unset means ws://127.0.0.1:9010/ (the live ui-server)"
    cb = env.get("CAULDRON_BIN", "").strip()
    if not cb or os.path.basename(cb).startswith("cauldron"):
        return f"CAULDRON_BIN={cb!r} is unset or a cauldron; the restart button would run `cauldron --env <env> start threads`"
    if env.get("CAULDRON_OPENER", "").strip() != "none":
        return f"CAULDRON_OPENER={env.get('CAULDRON_OPENER', '')!r} is not `none`; unset, `o` in the --cauldron face execs the system `open` on his desktop (IS-111)"
    if env.get("CAULDRON_TYPE_ONE", "").strip() != "off":
        return f"CAULDRON_TYPE_ONE={env.get('CAULDRON_TYPE_ONE', '')!r} is not `off`; otherwise the --cauldron seeding makes a real model call through his threads daemon or OpenRouter"
    # HOME (R7.S5b): under it the face reads `~/.cauldron` (the registry a
    # registry act REWRITES) and writes `~/.tui`. His home, by either name —
    # the account's, or the one this driver inherited — is refused.
    home = env.get("HOME", "").strip()
    real = {os.path.realpath(pwd.getpwuid(os.getuid()).pw_dir)}
    if os.environ.get("HOME"):
        real.add(os.path.realpath(os.environ["HOME"]))
    if not home or os.path.realpath(home) in real:
        return f"HOME={home!r} is unset or the operator's real home; the face would read his ~/.cauldron and write his ~/.tui"
    # CAULDRON_HOME (REL-1.S1c): the --cauldron face's store root, which it
    # WRITES. A scratch dir only: unset, relative, his home, inside his home
    # (his root is ~/.cauldron), or an ancestor of his home is refused.
    ch = env.get("CAULDRON_HOME", "").strip()
    if not ch or not os.path.isabs(ch):
        return f"CAULDRON_HOME={ch!r} is unset or relative; the --cauldron face would write its store under $HOME/.cauldron or wherever the parent pointed it"
    rch = os.path.realpath(ch)
    rhome = os.path.realpath(home).rstrip("/")
    if not rch.startswith(rhome + "/"):
        return f"CAULDRON_HOME={ch!r} is not inside the scratch HOME {home!r}; this driver's store root is always <HOME>/chome"
    for r in real:
        if rch == r or rch.startswith(r.rstrip("/") + "/") or r.startswith(rch.rstrip("/") + "/"):
            return f"CAULDRON_HOME={ch!r} is not a scratch dir: it is, is inside, or contains the operator's real home {r!r} (his live store is ~/.cauldron)"
    return None


COLS, ROWS, BIN, HOME = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4]
SCRIPT = sys.argv[5]          # a python expression: list of (bytes, wait_seconds)
chunks = eval(SCRIPT)          # noqa: S307 — this file's input is this file's author
PORT = threads_port(sys.argv[6] if len(sys.argv) > 6 else "ws://127.0.0.1:1/ws")

env = dict(os.environ)
env["TERM"] = "xterm-256color"
env["HOME"] = HOME
# NO LIVE DAEMON. Port 1 (nothing listens) unless the caller named a fake: the
# face renders "connecting", and a send is refused at the transport rather than
# reaching his threads. Both halves of the port lever, as `check_ports` wants.
# One `env[...] =` per variable, right here above the launch, because that is
# the form `tests/drive_isolation.rs` reads PER LAUNCH (R7.S5b).
env["TUI_THREADS_CHASSIS_PORT"] = str(PORT)
env["TUI_THREADS_PORT"] = str(PORT)
env["AUI_V2_URL"] = "ws://127.0.0.1:1/"
env["CAULDRON_BIN"] = "/usr/bin/false"
env["CAULDRON_OPENER"] = "none"
env["CAULDRON_TYPE_ONE"] = "off"
env["CAULDRON_HOME"] = os.path.join(HOME, "chome")
env.pop("TUI_THREADS_URL", None)   # retired; nothing reads it, so nothing may imply it isolates
env.pop("TMUX", None)      # the negotiation must not read a real tmux server
why = refuse(env)
if why:
    sys.stderr.write(f"pty_drive: REFUSED to start the face: {why}\n")
    sys.exit(3)

pid, fd = pty.fork()
if pid == 0:
    os.execve(BIN, [BIN], env)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

buf = b""
def drain(seconds):
    global buf
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                c = os.read(fd, 1 << 20)
            except OSError:
                return False
            if not c:
                return False
            buf += c
    return True

drain(1.2)                     # let the first frame land
# **AND IF NOTHING HAS BEEN PAINTED, KEEP WAITING RATHER THAN MEASURE AN EMPTY
# TERMINAL.** Measured 2026-09-28, chasing five `pointer_frames.sh` failures
# that a bisection had landed on a KEYBOARD feature: the FIRST execution of a
# freshly linked binary writes **zero** bytes for 0.3–0.6 s on a quiet machine,
# and longer on a loaded one. macOS validates the code signature of the whole
# ~10 MB image before `main` runs, and the result is cached per INODE — so the
# run right after `cargo build` pays it and every run after that is warm. A
# suite whose first drive lands inside that window reads a BLANK grid.
#
# What that costs is not a missed assertion, which would at least be honest. A
# shell that derives a row number from a blank grid gets `$(( "" - 1 ))` = -1,
# clicks at wire row 0, and reports four separate lies about the click path —
# `the click did not move the cursor (cursor on 5, clicked -1)` is what a fixed
# settle window looks like from the far end.
#
# `not buf` is the whole condition, deliberately. A face that has painted
# ANYTHING is a face whose process is running, and the 1.2 s above is left to
# cover the rest of its first frame. So this can only ever ADD waiting, and
# only in the one case where the alternative is measuring nothing at all: no
# suite that passes today can start failing because of it.
waited = 0.0
while not buf and waited < 8.0:
    if not drain(0.4):
        break                  # the child is gone; nothing will ever arrive
    waited += 0.4
if waited:
    drain(1.2)                 # it has only just started: give it the same settle
for data, wait in chunks:
    try:
        # Write in slices: a pty's input buffer is not unbounded, and an 800 KB
        # paste written in one call blocks forever on a reader that is busy.
        for i in range(0, len(data), 4096):
            os.write(fd, data[i:i+4096])
    except OSError:
        break
    if not drain(wait):
        break
try:
    os.write(fd, b"\x03")      # ^C quits, in every mode
except OSError:
    pass
drain(0.8)
try:
    os.kill(pid, 9)
except ProcessLookupError:
    pass
try:
    os.waitpid(pid, 0)
except ChildProcessError:
    pass

print("MODES " + " ".join(f"?{m.decode()}{a.decode()}"
                          for m, a in re.findall(rb"\x1b\[\?(\d+)([hl])", buf)))
print("OTHER " + " ".join(s.decode("latin1") for s in re.findall(rb"\x1b\[>[0-9;]*[a-zA-Z]", buf)))

grid = [[" "] * COLS for _ in range(ROWS)]
row = col = i = 0
while i < len(buf):
    b = buf[i]
    if b == 0x1B:
        # The parameter class includes `>` and `<` so `CSI > 4 ; 2 m` and
        # `CSI < u` are CONSUMED rather than falling through and printing
        # themselves onto the grid as text.
        m = re.match(rb"\x1b\[([0-9;?<>]*)([A-Za-z])", buf[i:])
        if m:
            params, final = m.group(1), m.group(2)
            if final == b"H":
                p = params.split(b";")
                row = (int(p[0]) - 1) if p and p[0] else 0
                col = (int(p[1]) - 1) if len(p) > 1 and p[1] else 0
            elif final == b"J" and params in (b"2", b""):
                grid = [[" "] * COLS for _ in range(ROWS)]
            i += m.end(); continue
        m = re.match(rb"\x1b\][^\x07\x1b]*(\x07|\x1b\\)", buf[i:])
        if m:
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
    col += 1
    i += ln
for r in grid:
    print("SCREEN " + "".join(r).rstrip())