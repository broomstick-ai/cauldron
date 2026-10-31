#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  DRAG-TO-COPY, PROVEN ON A REAL TERMINAL, WITH THE BYTES SHOWN.
#
#  `IS-21` is a defect about a byte stream: the face asked for `?1000h`, which
#  reports press and release and NO motion, so it could not see a drag — while
#  tmux, whose drag binding branches on `mouse_any_flag` (set by ?1000h too),
#  forwarded the drag to us and did no selection of its own. Neither side
#  selected. `IS-27` is a second byte-stream defect sitting behind it: the
#  app's `OSC 52` copy is discarded by `input_osc_52()` unless `set-clipboard`
#  is `on`, and he runs `external`.
#
#  Neither can be proven by a unit test, because both are about what leaves a
#  pty. So this drives REAL SGR bytes into a REAL pane and reads what comes out
#  of the far side of tmux.
#
#  Five things, in order:
#
#    1. A DRAG paints a selection. Synthetic `\e[<0;C;RM`, motion, `m` — the
#       exact bytes iTerm2 sends — and the pane comes back with the characters
#       reversed. Under `?1000h` this produced nothing at all.
#    2. CLICK AND DRAG ARE MUTUALLY EXCLUSIVE, both directions, on the SAME
#       cell: the drag must not activate, the click must.
#    3. THE COPY REACHES THE TERMINAL UNDER `set-clipboard external` — his
#       actual setting — and the base64 on the wire decodes to the LOGICAL
#       text, not to the painted screen.
#    4. THE FORCED FAILURE. The same copy written RAW, the way this face wrote
#       it until `0041`, reaches nothing under the same setting. That is what
#       makes (3) evidence rather than a green.
#    5. THE SIZE GUARD. A copy past the ceiling writes ZERO bytes to the wire
#       and says so, instead of handing tmux a sequence it discards in silence.
#
#  **It cannot touch his tmux.** `-L tuidrag` is a private tmux SERVER socket:
#  a different server, a different socket path, invisible to his sessions, and
#  killed on the way out. Every option it sets is set on THAT server. His
#  `~/.tmux.conf` is never read (`-f /dev/null`) and never written.
#
#  **It cannot touch his daemon.** The face's endpoint variables
#  (`TUI_THREADS_CHASSIS_PORT`, `TUI_THREADS_PORT`, `AUI_V2_URL`,
#  `CAULDRON_BIN`) are pointed at the crate's own `fake_daemon` on a loopback
#  port / port 1 / `/usr/bin/false`, and its HOME is a scratch directory;
#  `tests/drive_isolation.rs` pins that set on every launch.
#
#  Usage:  ./drag_frames.sh [output-file]
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

BIN="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/tui"
CLIP="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/examples/clip_probe"
OUT="${1:-${CARGO_TARGET_DIR:-$REPO_ROOT/target}/tui-drag-frames.txt}"
SOCK=tuidrag
S="tmux -L $SOCK -f /dev/null"
WORK="$(mktemp -d)"
FAILURES=0
# **A SCRATCH HOME (R7.S5b).** The face resolves `~` once (`dirs::home_dir()`)
# and under it reads `~/.cauldron` — the project registry, which a registry act
# REWRITES (`~/.cauldron/projects.json`, read by the live ui-server) — and
# writes `~/.tui/log` and `~/.tui/diag.log` beside his live face's. This used to
# inherit the real HOME. One project, `harness`, because every section below
# drags or clicks that row — and no threads in it: a thread would put a SECOND
# `  harness ` cell on screen, in the recent band, and the row lookup would
# read two lines. `.tui` exists so this is not a first run.
FAKEHOME="$WORK/home"
mkdir -p "$FAKEHOME/.tui" "$FAKEHOME/minds/harness/.mind"
cat > "$FAKEHOME/minds/harness/.mind/threads.json" <<'JSON'
{"threads":[]}
JSON

FAKE_PID=""
cleanup() {
    $S kill-server 2>/dev/null
    [ -n "$FAKE_PID" ] && kill "$FAKE_PID" 2>/dev/null
    rm -rf "$WORK"
}
# The scratch target tree goes with it: bash's traps are last-write-wins, so a
# suite that installs its own EXIT trap would otherwise silently discard the
# one `conformance_target.sh` set and leave half a gigabyte behind.
trap 'cleanup; conformance_target_release' EXIT INT TERM

ok()    { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
bad()   { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
head_() { printf '\n\033[1m%s\033[0m\n' "$1"; }

(cd "$REPO_ROOT" && cargo build -p tui --example clip_probe >/dev/null 2>&1)
(cd "$REPO_ROOT" && cargo build -p tui >/dev/null 2>&1)
[ -x "$BIN" ]  || { echo "build it first: cargo build -p tui"; exit 1; }
[ -x "$CLIP" ] || { echo "build it first: cargo build -p tui --example clip_probe"; exit 1; }

# ── A FAKE DAEMON, because the root needs one (R7.S5b) ───────────────────────
# Sections 1, 2 and 2b drag and click the PROJECT LIST. Until R7.S5 the face
# here reached production threads (3653) — that is the only reason the project
# list was on screen at all. Pointed at port 1 it paints "threads is down"
# instead, and those sections measured a screen with no `harness` row on it
# (measured on R7.S5's tip: header `? unread · … disconnected`, harness row
# -1, five FAILs). So the face gets the crate's own `examples/fake_daemon` on a
# loopback port, exactly as `pointer_frames.sh` §1b does — it binds 127.0.0.1
# and answers out of memory; it is not his daemon and cannot reach one.
FAKE_PORT="${TUI_FAKE_PORT:-18793}"
case "$FAKE_PORT" in
    3651|3653|3753|9010) echo "REFUSING: TUI_FAKE_PORT=$FAKE_PORT is a LIVE port"; exit 3 ;;
esac
port_listening() { python3 -c 'import socket,sys
s = socket.socket(); s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1", int(sys.argv[1]))) == 0 else 1)' "$1"; }
if port_listening "$FAKE_PORT"; then
    echo "ABORTING: 127.0.0.1:$FAKE_PORT is already taken — a stale fake_daemon?"
    echo "  Without OUR fake daemon the face would talk to whatever holds it. Kill"
    echo "  the holder or set TUI_FAKE_PORT to a free port."
    exit 1
fi
(cd "$REPO_ROOT" && cargo build -p tui --example fake_daemon >/dev/null 2>&1)
"${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/examples/fake_daemon" "$FAKE_PORT" >"$WORK/fake.log" 2>&1 &
FAKE_PID=$!
BOUND=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    kill -0 "$FAKE_PID" 2>/dev/null || break
    if port_listening "$FAKE_PORT"; then BOUND=1; break; fi
    sleep 0.5
done
if [ "$BOUND" -ne 1 ]; then
    sed 's/^/    /' "$WORK/fake.log" 2>/dev/null | head -20
    echo "ABORTING: the fake daemon never listened on 127.0.0.1:$FAKE_PORT; every"
    echo "  section below would read a 'threads is down' screen."
    exit 1
fi
: > "$OUT"

# ── the outer-pty reader ─────────────────────────────────────────────────────
# A tmux CLIENT under a pty we own, so what tmux writes toward "the terminal"
# is a file we can read. This is the only way to see whether a sequence the
# pane emitted actually left tmux.
cat > "$WORK/outer.py" <<'PY'
import os, pty, select, sys, termios, fcntl, struct, time
COLS, ROWS, SECS, DUMP = 120, 30, float(sys.argv[1]), sys.argv[2]
cmd = sys.argv[3:]
pid, fd = pty.fork()
if pid == 0:
    env = dict(os.environ); env["TERM"] = "xterm-256color"
    os.execvp(cmd[0], cmd)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
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

# ── reading an OSC 52 off that dump ──────────────────────────────────────────
# The whole question is "did the sequence leave tmux", and the honest answer is
# a decode: a HIT that does not decode to what was sent is not a hit.
cat > "$WORK/osc.py" <<'PY'
import base64, pathlib, re, sys
buf = pathlib.Path(sys.argv[1]).read_bytes()
m = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(\x07|\x1b\\)", buf)
if not m:
    print("MISS")
else:
    try:
        print("HIT " + base64.b64decode(m.group(1)).decode().replace("\n", " / "))
    except Exception as e:
        print("HIT <undecodable: %s>" % e)
PY

cat > "$WORK/show.py" <<'PY'
import base64, pathlib, re, sys
buf = pathlib.Path(sys.argv[1]).read_bytes()
m = re.search(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)(\x07|\x1b\\)", buf)
if not m:
    print("NO OSC 52 ON THE WIRE")
else:
    payload = m.group(1)
    print("bytes on the wire: " + repr(m.group(0)[:100]))
    print("base64:            " + payload.decode())
    print("decodes to:        " + repr(base64.b64decode(payload).decode()))
    print("padded:            " + ("yes" if len(payload) % 4 == 0 else "NO - tmux b64_pton would refuse"))
PY

# ── SGR injection ────────────────────────────────────────────────────────────
# ESC [ < btn ; col ; row M|m  — 1-based on the wire, SGR 1006. Motion while
# button 0 is held is button code 32 (0 + 32), which is what `?1002h` reports
# and `?1000h` never does.
# Arguments are COL then ROW, 0-based, and they are named in every call site —
# the wire order is the other way round and a silent swap here produces a
# gesture that still passes while proving something else entirely.
hexof() { printf '%s' "$1" | xxd -p -c1 | tr '\n' ' '; }
press()  { $S send-keys -t probe -H $(hexof "$(printf '\033[<0;%d;%dM'  $(( $1 + 1 )) $(( $2 + 1 )))"); }
motion() { $S send-keys -t probe -H $(hexof "$(printf '\033[<32;%d;%dM' $(( $1 + 1 )) $(( $2 + 1 )))"); }
release(){ $S send-keys -t probe -H $(hexof "$(printf '\033[<0;%d;%dm'  $(( $1 + 1 )) $(( $2 + 1 )))"); }

shot() { printf '\n=== %s ===\n' "$1" >> "$OUT"; $S capture-pane -p -t probe >> "$OUT"; }

start_face() {
    # `kill-server` returns before the socket is gone, and a `new-session`
    # that races it lands on a server that is still shutting down — which
    # showed up as an intermittent "no server running" three sections later,
    # reported as a defect in the face. Wait for the socket, then assert the
    # session is really there before anything is measured against it.
    $S kill-server 2>/dev/null
    sleep 1
    # `-e` sets the PANE's environment whatever the server inherited. These
    # are the variables the face READS (R7.S5): this used to prefix only
    # `TUI_THREADS_URL`, which nothing reads, so the face's seam defaulted to
    # 3653, production threads. HOME is the scratch home above (R7.S5b).
    # Pinned, on THIS launch, by `tests/drive_isolation.rs`.
    $S new-session -d -s probe -x 120 -y 30 \
        -e "HOME=$FAKEHOME" \
        -e "TUI_THREADS_CHASSIS_PORT=$FAKE_PORT" -e "TUI_THREADS_PORT=$FAKE_PORT" \
        -e "AUI_V2_URL=ws://127.0.0.1:1/" -e "CAULDRON_BIN=/usr/bin/false" \
        -e "CAULDRON_OPENER=none" -e "CAULDRON_TYPE_ONE=off" \
        -e "CAULDRON_HOME=$WORK/chome" \
        "$BIN"
    # HIS SETTINGS, on a server that is not his. `external` is the one that
    # drops a raw OSC 52; `allow-passthrough on` is what he already has.
    $S set-option -g set-clipboard external
    $S set-option -g allow-passthrough on
    sleep 6
    if ! $S has-session -t probe 2>/dev/null; then
        bad "the probe session did not come up — nothing below this is evidence"
        return 1
    fi
}

# A tmux CLIENT under a pty we own, running in the background for `$1` seconds.
# Everything tmux writes toward "the terminal" lands in $WORK/outer.bin.
watch_outer() {
    rm -f "$WORK/outer.bin"
    python3 "$WORK/outer.py" "$1" "$WORK/outer.bin" \
        tmux -L "$SOCK" -f /dev/null attach -t "$2" &
    OUTER_PID=$!
}

osc52_of() { python3 "$WORK/osc.py" "$1"; }

# ─────────────────────────────────────────────────────────────────────────────
head_ "1. a real drag on a real pane — the selection paints"
# ─────────────────────────────────────────────────────────────────────────────
start_face
watch_outer 16 probe
sleep 1
shot "0. the root, before any gesture"
printf '(tmux %s, set-clipboard=%s allow-passthrough=%s)\n' \
    "$($S -V)" "$($S show-options -gv set-clipboard)" "$($S show-options -gv allow-passthrough)" >> "$OUT"

ROW=$($S capture-pane -p -t probe | grep -n '  harness ' | cut -d: -f1)
ROW=$(( ROW - 1 ))
printf '(the harness row is screen row %s)\n' "$ROW" >> "$OUT"

# Drag across that row. `-e` on capture-pane keeps the escape sequences, so the
# REVERSE VIDEO the selection paints is visible as bytes rather than guessed at.
press 2 "$ROW"; sleep 0.3
motion 10 "$ROW"; sleep 0.3
motion 24 "$ROW"; sleep 0.5
$S capture-pane -p -e -t probe > "$WORK/mid.txt"
shot "1. mid-drag, plain capture"
printf '\n=== 1b. mid-drag, WITH escapes (row %s) ===\n' "$ROW" >> "$OUT"
sed -n "$(( ROW + 1 ))p" "$WORK/mid.txt" | cat -v >> "$OUT"

if sed -n "$(( ROW + 1 ))p" "$WORK/mid.txt" | grep -q '\[7m'; then
    ok "the drag painted a selection (SGR 7, reverse video, on the dragged row)"
    sed -n "$(( ROW + 1 ))p" "$WORK/mid.txt" | cat -v | head -c 200 | sed 's/^/        /'
    printf '\n'
else
    bad "no reverse video on the dragged row — the selection did not paint"
    sed -n "$(( ROW + 1 ))p" "$WORK/mid.txt" | cat -v | head -c 200 | sed 's/^/        /'
    printf '\n'
fi

release 24 "$ROW"; sleep 1.0
shot "2. after the release — the highlight is kept, and the footer says what was copied"
FOOT="$($S capture-pane -p -t probe | tail -1)"
printf '(footer: %s)\n' "$FOOT" >> "$OUT"
case "$FOOT" in
    *"chars copied"*) ok "the footer names the copy: $FOOT" ;;
    *) bad "the release said nothing about a copy: $FOOT" ;;
esac

# …and the CLIPBOARD BYTES that drag put on the wire, decoded. This is the
# whole gesture end to end: real SGR in, real OSC 52 out of tmux, and the
# payload is the LOGICAL text — a row's own characters, with no marker column
# and none of the right-hand metadata the row is padded out with.
wait $OUTER_PID 2>/dev/null
DRAG_CLIP="$(osc52_of "$WORK/outer.bin")"
printf '\n=== 2b. what that drag put on the wire ===\n%s\n' "$DRAG_CLIP" >> "$OUT"
case "$DRAG_CLIP" in
    HIT*harness*)
        ok "the drag's OSC 52 left tmux and decodes to the rows it crossed"
        printf '        %s\n' "$(printf '%s' "$DRAG_CLIP" | head -c 160)"
        ;;
    MISS) bad "the drag copied nothing onto the wire" ;;
    *) bad "the drag's copy does not contain the row it crossed: $(printf '%s' "$DRAG_CLIP" | head -c 160)" ;;
esac

# ─────────────────────────────────────────────────────────────────────────────
head_ "2. click and drag are mutually exclusive, on the same cell"
# ─────────────────────────────────────────────────────────────────────────────
# A DRAG on the project row must NOT open the project — and since `0052` a
# CLICK must not open it either. His retraction, verbatim: "a click on a row
# moves the cursor to that row. It does not drill." So the assertion below is
# INVERTED from what it was, and the two-step grammar is proven after it: the
# click puts the cursor on the row, and `→` is what descends.
start_face
ROW=$($S capture-pane -p -t probe | grep -n '  harness ' | cut -d: -f1); ROW=$(( ROW - 1 ))
# **"STILL ON THE ROOT" IS "THE HEADER HAS NOT CHANGED" (R7.S5b).** These
# checks matched `projects*` against the first screen line. That line is the
# status band (`? unread · ? active · ? queued · connected`) and has been for
# some time, so the two "did not descend" checks could only FAIL and the "→
# descends" check could only PASS — it passed on any header at all (measured on
# this tip: all three said so with the face never having moved). The root's own
# header is captured here, and a descent is a header that differs from it.
ROOT_HEAD="$($S capture-pane -p -t probe | head -1)"
DESCENDED_HEAD="harness ·"
case "$ROOT_HEAD" in
    "$DESCENDED_HEAD"*|"") bad "the root header is not a root header: ${ROOT_HEAD:0:60}" ;;
esac
press 6 "$ROW"; sleep 0.2; motion 30 "$ROW"; sleep 0.2; release 30 "$ROW"; sleep 2.0
shot "3. a DRAG across a project row — it must not descend"
HEAD_AFTER_DRAG="$($S capture-pane -p -t probe | head -1)"
case "$HEAD_AFTER_DRAG" in
    "$ROOT_HEAD") ok "a drag did NOT activate — still on the root (header: ${HEAD_AFTER_DRAG:0:40}…)" ;;
    *) bad "a drag ACTIVATED: the face descended (header: ${HEAD_AFTER_DRAG:0:60})" ;;
esac

press 6 "$ROW"; sleep 0.2; release 6 "$ROW"; sleep 2.5
shot "4. a CLICK on the same row — it SELECTS, it does not descend (0052)"
HEAD_AFTER_CLICK="$($S capture-pane -p -t probe | head -1)"
case "$HEAD_AFTER_CLICK" in
    "$ROOT_HEAD") ok "a click did NOT drill — still on the root, as he ruled (header: ${HEAD_AFTER_CLICK:0:40}…)" ;;
    *) bad "a click DRILLED IN: the retraction is not in force (header: ${HEAD_AFTER_CLICK:0:60})" ;;
esac

# …and the cursor MOVED to the row it clicked, which is the whole of what a
# click now does. The cursor is drawn as a `▸` prefix on the selected row.
CURSOR_ROW=$($S capture-pane -p -t probe | grep -n '^▸' | head -1 | cut -d: -f1)
CURSOR_ROW=$(( ${CURSOR_ROW:-0} - 1 ))
if [ "$CURSOR_ROW" = "$ROW" ]; then
    ok "the click MOVED THE CURSOR to the row it landed on (screen row $ROW)"
else
    bad "the click did not move the cursor: cursor is on row $CURSOR_ROW, clicked $ROW"
fi

# THE SECOND STEP. `→` on the row the click picked is what opens it.
$S send-keys -t probe Right; sleep 2.5
shot "4b. → after the click — NOW it descends"
HEAD_AFTER_RIGHT="$($S capture-pane -p -t probe | head -1)"
case "$HEAD_AFTER_RIGHT" in
    "$DESCENDED_HEAD"*) ok "→ after the click DID descend — the two-step grammar (header: ${HEAD_AFTER_RIGHT:0:40}…)" ;;
    *) bad "→ after a click did not descend into harness (header: ${HEAD_AFTER_RIGHT:0:60})" ;;
esac

# ─────────────────────────────────────────────────────────────────────────────
head_ "2b. a header cell and a footer cell are selectable too"
# ─────────────────────────────────────────────────────────────────────────────
# "ANY part of the interface" — and the header and the footer are the two bands
# most likely to have been forgotten, because neither is the transcript.
start_face
watch_outer 12 probe
sleep 1
press 0 0; sleep 0.2; motion 20 0; sleep 0.4; release 20 0; sleep 0.8
shot "5. a drag across the HEADER row"
wait $OUTER_PID 2>/dev/null
HDR="$(osc52_of "$WORK/outer.bin")"
printf '\n=== 5b. the header drag put this on the wire ===\n%s\n' "$HDR" >> "$OUT"
case "$HDR" in
    # The header band's own words (was `projects`, a layout since replaced).
    HIT*unread*active*) ok "a drag on the HEADER copied the header: $(printf '%s' "$HDR" | head -c 90)" ;;
    *) bad "a header drag copied nothing usable: $(printf '%s' "$HDR" | head -c 90)" ;;
esac

watch_outer 12 probe
sleep 1
press 2 29; sleep 0.2; motion 20 29; sleep 0.4; release 20 29; sleep 0.8
shot "6. a drag across the FOOTER row"
wait $OUTER_PID 2>/dev/null
FTR="$(osc52_of "$WORK/outer.bin")"
printf '\n=== 6b. the footer drag put this on the wire ===\n%s\n' "$FTR" >> "$OUT"
case "$FTR" in
    HIT*keys*) ok "a drag on the FOOTER copied the footer: $(printf '%s' "$FTR" | head -c 90)" ;;
    *) bad "a footer drag copied nothing usable: $(printf '%s' "$FTR" | head -c 90)" ;;
esac

# ─────────────────────────────────────────────────────────────────────────────
head_ "3. the copy reaches the terminal under set-clipboard external"
# ─────────────────────────────────────────────────────────────────────────────
clip_run() { # <mode> <text> <set-clipboard>  -> writes $WORK/outer.bin
    $S kill-server 2>/dev/null
    rm -f "$WORK/verdict.txt"
    # A SHELL in the pane, not a command: `send-keys` types AT A SHELL, and a
    # pane running `sleep` would have swallowed every one of these runs while
    # reporting nothing — which is how the first version of this script
    # produced four confident MISSes about code that works.
    $S new-session -d -s clip -x 120 -y 30
    $S set-option -g set-clipboard "$3"
    $S set-option -g allow-passthrough on
    sleep 1
    watch_outer 8 clip
    sleep 2
    $S send-keys -t clip "$CLIP $1 '$2' 2>$WORK/verdict.txt" Enter
    wait $OUTER_PID 2>/dev/null
    $S kill-server 2>/dev/null
}

PAYLOAD='the logical text, unwrapped'
clip_run wrapped "$PAYLOAD" external
printf '\n=== 7. OSC 52 on the OUTER pty, wrapped, set-clipboard=external ===\n' >> "$OUT"
python3 "$WORK/show.py" "$WORK/outer.bin" >> "$OUT"
RESULT="$(osc52_of "$WORK/outer.bin")"
if [ "$RESULT" = "HIT $PAYLOAD" ]; then
    ok "the wrapped copy REACHED the terminal under external, and decodes to the text sent"
    printf '        %s\n' "$RESULT"
else
    bad "the wrapped copy did not reach the terminal under external: $RESULT"
fi
printf '        probe said: %s\n' "$(cat "$WORK/verdict.txt" 2>/dev/null)"

# ─────────────────────────────────────────────────────────────────────────────
head_ "4. FORCED FAILURE — the raw path, the way it shipped, under the same setting"
# ─────────────────────────────────────────────────────────────────────────────
clip_run raw "$PAYLOAD" external
RAW_RESULT="$(osc52_of "$WORK/outer.bin" | cut -d' ' -f1)"
printf '\n=== 8. the RAW path, set-clipboard=external: %s ===\n' "$RAW_RESULT" >> "$OUT"
if [ "$RAW_RESULT" = "MISS" ]; then
    ok "the raw copy reached NOTHING — which is IS-27, and is why the wrapper is load-bearing"
else
    bad "the raw copy reached the terminal too — section 3 proves nothing about the wrapper"
fi

# …and the wrapper works under the other two settings as well, which is what
# makes it independent of his config rather than tuned to it.
for MODE in on off; do
    clip_run wrapped "$PAYLOAD" "$MODE"
    R="$(osc52_of "$WORK/outer.bin" | cut -d' ' -f1)"
    printf '\n=== 9. wrapped, set-clipboard=%s: %s ===\n' "$MODE" "$R" >> "$OUT"
    if [ "$R" = "HIT" ]; then
        ok "wrapped reaches the terminal under set-clipboard=$MODE too (nothing depends on his config)"
    else
        bad "wrapped did NOT reach under set-clipboard=$MODE"
    fi
done

# ─────────────────────────────────────────────────────────────────────────────
head_ "5. the size guard — an honest refusal, and ZERO bytes on the wire"
# ─────────────────────────────────────────────────────────────────────────────
clip_run oversize "" external
BIG_RESULT="$(osc52_of "$WORK/outer.bin" | cut -d' ' -f1)"
VERDICT="$(cat "$WORK/verdict.txt" 2>/dev/null)"
printf '\n=== 10. oversize: wire=%s verdict=%s ===\n' "$BIG_RESULT" "$VERDICT" >> "$OUT"
if [ "$BIG_RESULT" = "MISS" ] && case "$VERDICT" in *refused*) true ;; *) false ;; esac; then
    ok "an over-ceiling copy wrote NOTHING and refused by name"
    printf '        %s\n' "$VERDICT"
else
    bad "an over-ceiling copy was not refused cleanly (wire=$BIG_RESULT verdict=$VERDICT)"
fi

head_ "result"
printf '  frames captured to %s\n' "$OUT"
if [ "$FAILURES" -eq 0 ]; then
    printf '  \033[32mdrag-to-copy proven on a real pty, with the forced failure observed\033[0m\n\n'
    exit 0
fi
printf '  \033[31m%d check(s) failed\033[0m\n\n' "$FAILURES"
exit 1
