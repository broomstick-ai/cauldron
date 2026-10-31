#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  Drive the REAL `tui` binary against the LIVE threads daemon and capture the
#  rendered screens verbatim.
#
#  Golden tests prove the face draws what the model says. They cannot prove the
#  model is the daemon's — only a run against the real thing does that, and the
#  only honest way to show a terminal face is the characters it actually put on
#  a terminal. So: a real pty, at a real size, captured as text.
#
#  **READ-ONLY.** It sends only navigation keys. The face itself cannot issue a
#  mutating verb — `chat_conformance_test.sh` §7 checks the shipped source for
#  all of them, `seen` included — so there is no key sequence here that
#  could reach one.
#
#  **It cannot touch his tmux.** `-L tuiprobe` is a private tmux SERVER socket:
#  a different server, a different socket path, invisible to his sessions and
#  killed on the way out.
#
#  Usage:  ./live_frames.sh [output-file]
# ─────────────────────────────────────────────────────────────────────────────
set -u
CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
BIN="$REPO_ROOT/target/debug/tui"
OUT="${1:-$REPO_ROOT/target/tui-live-frames.txt}"
S="tmux -L tuiprobe"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM

[ -x "$BIN" ] || { echo "build it first: cargo build -p tui"; exit 1; }
: > "$OUT"

# ── a real DRAG on the live transcript, and the bytes it puts on the wire ────
# The gesture is the round's subject and it cannot be shown by a screen capture
# alone: what makes it right is the CLIPBOARD, and the clipboard leaves through
# the outer pty. So a tmux client runs under a pty we own and the OSC 52 is
# read off it and decoded.
cat > "$WORK/outer.py" <<'PY'
import os, pty, select, sys, termios, fcntl, struct, time
SECS, DUMP = float(sys.argv[1]), sys.argv[2]
pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    os.execvp(sys.argv[3], sys.argv[3:])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 150, 0, 0))
buf, deadline = b"", time.time() + SECS
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.2)
    if r:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
open(DUMP, "wb").write(buf)
PY
cat > "$WORK/osc.py" <<'PY'
import base64, pathlib, re, sys
buf = pathlib.Path(sys.argv[1]).read_bytes()
m = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(\x07|\x1b\\)", buf)
print("MISS" if not m else "HIT " + repr(base64.b64decode(m.group(1)).decode()))
PY

hexof() { printf '%s' "$1" | xxd -p -c1 | tr '\n' ' '; }
# COL then ROW, 0-based. Button 32 is motion-while-held, which only `?1002h`
# reports — under `?1000h` it never arrives, and that is IS-21.
press()  { $S send-keys -t probe -H $(hexof "$(printf '\033[<0;%d;%dM'  $(( $1 + 1 )) $(( $2 + 1 )))"); }
motion() { $S send-keys -t probe -H $(hexof "$(printf '\033[<32;%d;%dM' $(( $1 + 1 )) $(( $2 + 1 )))"); }
release(){ $S send-keys -t probe -H $(hexof "$(printf '\033[<0;%d;%dm'  $(( $1 + 1 )) $(( $2 + 1 )))"); }

shot() {
    printf '\n=== %s ===\n' "$1" >> "$OUT"
    $S capture-pane -p -t probe >> "$OUT"
}
start() {
    $S kill-server 2>/dev/null
    $S new-session -d -s probe -x "$1" -y "$2" "$BIN" 2>/dev/null \
        || { echo "could not start a probe session"; exit 1; }
    # The first paint waits on a real round trip to his daemon
    # (`threads/status` and `threads/list` over a thousand threads), so this is a real wait and not
    # a nervous one.
    sleep 8
}

# **LEFT AND RIGHT ONLY, all the way down.** The descent below uses no key
# other than the arrows — which is the point of it. `project → thread →
# transcript` with `→`, and back out with `←`.

start 150 40
shot "1. THE ROOT IS PROJECTS — the face as it opens (150x40)"

# Twelve rows down is `harness`, which has a real index and real threads.
for _ in $(seq 12); do $S send-keys -t probe Down; done ; sleep 1
shot "2. the root, cursor moved with the arrows"

$S send-keys -t probe Right      ; sleep 4   # DRILL IN: a project's threads
shot "3. → into a project — ITS threads, out of its own .mind/threads.json"

$S send-keys -t probe Right      ; sleep 7   # DRILL IN: the transcript
shot "4. → into a thread — the chat, newest at the bottom"

# ── THE DRAG, on a live transcript ──────────────────────────────────────────
rm -f "$WORK/outer.bin"
python3 "$WORK/outer.py" 14 "$WORK/outer.bin" tmux -L tuiprobe attach -t probe &
READER=$!
sleep 2
# Sweep several rows of the chat: down and to the right, which is how a hand
# actually moves across a transcript.
press 6 12; sleep 0.3
motion 40 15; sleep 0.3
motion 90 20; sleep 0.6
shot "4b. MID-DRAG — the selection painted over a live transcript"
$S capture-pane -p -e -t probe | sed -n '13,21p' | cat -v >> "$OUT"
release 90 20; sleep 1
shot "4c. after the release — the footer says what went to the clipboard"
wait $READER 2>/dev/null
printf '\n=== 4d. THE CLIPBOARD, read off the outer pty and decoded ===\n' >> "$OUT"
python3 "$WORK/osc.py" "$WORK/outer.bin" >> "$OUT"

$S send-keys -t probe Right      ; sleep 3
shot "5. → on a folded row — it expands (x is cut; this is the activation)"

# **6 AND 7 USED TO BE THE FILTER OVERLAY AND A HIDDEN LANE.** Round six deleted
# the filter — *"filter is too much. Just drop that feature."* — and kept the
# one-key collapse he actually asked for. These two shots are the collapse and
# the way back out of it, which is the whole of what replaced nine lane digits.
#
# **AND THE KEY IS `^S`, NOT `M-s`, SINCE ROUND SIX.** His ruling: *"ctrl-*
# might be better because it doesn't have keybindings and it's universally
# understood to work."* macOS composes `opt-s` into `ß` in the keyboard layer,
# so `M-s` was a key that never reached this face on his machine — a live
# script driving it was driving nothing.
$S send-keys -t probe Escape     ; sleep 2   # out of the chat, into the box
$S send-keys -t probe C-s        ; sleep 3
shot "6. stdout only (^S) — everything but the boxes held back"

$S send-keys -t probe C-s        ; sleep 3
shot "7. the same thread with everything back (^S again)"

# `u` is the BARE letter, and it is bound only where there is nothing to type
# into. From inside the thread the queue is `^O` (`^U` was taken — PageUp in
# the list modes, kill-to-start in the box).
$S send-keys -t probe C-o        ; sleep 4
shot "8. the queue overlay (^O from inside a thread)"

$S send-keys -t probe Left       ; sleep 1
$S send-keys -t probe Left       ; sleep 2
shot "9. ← back out of the thread, onto the project's list"

$S send-keys -t probe Left       ; sleep 2
shot "10. ← again — back at the ROOT, on the project he was in"

$S send-keys -t probe Tab        ; sleep 5
shot "11. the event lane (tab) — HIS threads only, not the daemon's firehose"

$S send-keys -t probe "?"        ; sleep 2
shot "12. the key overlay — the shared grammar printed ONCE"

$S send-keys -t probe Escape     ; sleep 1
start 80 24
shot "13. the root at 80x24"
$S send-keys -t probe Right      ; sleep 4
shot "14. a project's threads at 80x24"

$S send-keys -t probe q          ; sleep 1
$S kill-server 2>/dev/null
echo "captured to $OUT"
