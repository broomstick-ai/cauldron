#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  THE MOUSE, PROVEN ON A REAL TERMINAL.
#
#  `hit::activation` is unit-tested and the hit map is golden-tested, but
#  neither proves that a real SGR mouse report, written by a real terminal
#  emulator into the face's own pty, reaches the arm those tests describe. The
#  gap between the two is where the whole gesture lives, and it is exactly the
#  gap that hid a dead `→` for a build: the table had the binding, the crossterm
#  translation did not have the key, and nothing said so.
#
#  So: real bytes in, real screen out. Five things it shows, in order —
#
#    1. a PRESS ALONE does nothing. Activation fires on RELEASE, because a
#       click and the start of a drag are byte-identical at press time.
#    2. the RELEASE **SELECTS** the project — it does NOT descend. `0052`, on
#       his retraction of `DIR-13`: "a click on a row moves the cursor to that
#       row. It does not drill." `→` is what opens what the click picked.
#    3. a click on the FOOTER does nothing AND SAYS NOTHING. A miss must be
#       indistinguishable from not having clicked.
#    4. a click on the `▲` queue badge raises the queue — `DIR-6`'s "visible at
#       all times" badge made REACHABLE at all times, at the column it is
#       actually drawn at rather than a predicted one.
#    5. a click anywhere inside an overlay dismisses it, exactly as the
#       overlay's existing any-key binding already does.
#
#  **READ-ONLY.** Mouse reports and one `q`. The face cannot issue a mutating
#  verb at all — `chat_conformance_test.sh` §7 checks the shipped source for
#  every one of them.
#
#  **It cannot touch his daemon, or his home.** See `start()`: the face's
#  endpoint variables point at the crate's own fake daemon / port 1 and its
#  HOME is a scratch directory.
#
#  **It cannot touch his tmux.** `-L tuiclick` is a private tmux SERVER socket:
#  a different server, a different socket path, invisible to his sessions, and
#  killed on the way out.
#
#  **THIS SCRIPT CAPTURES FRAMES; IT DOES NOT ASSERT.** The assertions for the
#  same grammar live in `pointer_frames.sh` (which also covers the caret, the
#  motion keys and the wheel) and in `drag_frames.sh` §2. This one exists to be
#  LOOKED AT when something about the mouse feels wrong.
#
#  Usage:  ./click_frames.sh [output-file]
# ─────────────────────────────────────────────────────────────────────────────
set -u
CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
# ── BUILD INTO A CONFORMANCE-ONLY TARGET DIRECTORY ───────────────────────────
# The operator's LIVE face executes `$REPO_ROOT/target/debug/tui`, and every
# cargo invocation below rebuilds whatever `CARGO_TARGET_DIR` names. Sourcing
# this exports a target directory that is NOT the live one and REFUSES to run
# if the resolved one ever is. The whole argument is in that file.
. "$CRATE_DIR/conformance_target.sh"

BIN="${CARGO_TARGET_DIR}/debug/tui"
OUT="${1:-$REPO_ROOT/target/tui-click-frames.txt}"
[ -x "$BIN" ] || { echo "build it first: CARGO_TARGET_DIR=$CARGO_TARGET_DIR cargo build -p tui"; exit 1; }
S="tmux -L tuiclick"
# **A SCRATCH HOME (R7.S5b).** The face resolves `~` once, through
# `dirs::home_dir()`, and under it reads `~/.cauldron` (the project registry,
# which a registry act REWRITES — `~/.cauldron/projects.json`, the file the live
# ui-server reads) and writes `~/.tui/log` and `~/.tui/diag.log` beside his live
# face's. This used to inherit the real HOME. One project, `harness`, so the
# frames below have the row they click (no threads: a thread would put a SECOND
# `  harness ` cell on screen, in the recent band, and the row lookup below
# would read two lines); `.tui` exists so this is not a first run.
WORK="$(mktemp -d)"
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
# Chains `conformance_target.sh`'s release: bash traps are last-write-wins.
trap 'cleanup; conformance_target_release' EXIT INT TERM

# **A FAKE DAEMON (R7.S5b), as in `drag_frames.sh`.** Pointed at port 1 the face
# paints "threads is down" and none of the rows these frames click exist; until
# R7.S5 they existed only because the face was talking to production threads.
# The crate's own `examples/fake_daemon` binds 127.0.0.1 and answers from memory.
FAKE_PORT="${TUI_FAKE_PORT:-18794}"
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
"${CARGO_TARGET_DIR}/debug/examples/fake_daemon" "$FAKE_PORT" >"$WORK/fake.log" 2>&1 &
FAKE_PID=$!
BOUND=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    kill -0 "$FAKE_PID" 2>/dev/null || break
    if port_listening "$FAKE_PORT"; then BOUND=1; break; fi
    sleep 0.5
done
[ "$BOUND" -eq 1 ] || { echo "ABORTING: the fake daemon never listened on 127.0.0.1:$FAKE_PORT"; exit 1; }
: > "$OUT"
# ESC [ < 0 ; col ; row M|m   — 1-based, SGR 1006.
hexof() { printf '%s' "$1" | xxd -p -c1 | tr '\n' ' '; }
sgr() { # row col M|m   (0-based in, 1-based on the wire)
    local seq; seq=$(printf '\033[<0;%d;%d%s' $(( $2 + 1 )) $(( $1 + 1 )) "$3")
    $S send-keys -t probe -H $(hexof "$seq")
}
# NO DAEMON (R7.S5): this started the face with NOTHING set, so its seam went
# to 3653 — production threads — and its second socket to the live 9010. The
# variables the face reads now point at the fake daemon above / port 1, and
# HOME at the scratch home (R7.S5b); `tests/drive_isolation.rs` pins the set ON THIS LAUNCH.
start() {
    $S kill-server 2>/dev/null
    $S new-session -d -s probe -x 120 -y 30 \
        -e "HOME=$FAKEHOME" \
        -e "TUI_THREADS_CHASSIS_PORT=$FAKE_PORT" -e "TUI_THREADS_PORT=$FAKE_PORT" \
        -e "AUI_V2_URL=ws://127.0.0.1:1/" -e "CAULDRON_BIN=/usr/bin/false" \
        -e "CAULDRON_ISOLATED=1" -e "CAULDRON_ISOLATED_PORTS=127.0.0.1:$FAKE_PORT@$FAKE_PID" \
        -e "CAULDRON_OPENER=none" -e "CAULDRON_TYPE_ONE=off" \
        -e "CAULDRON_HOME=$WORK/chome" \
        "$BIN"
    sleep 8
}
shot() { printf '\n=== %s ===\n' "$1" >> "$OUT"; $S capture-pane -p -t probe >> "$OUT"; }

start
shot "0. the root, before any click"
ROW=$($S capture-pane -p -t probe | grep -n '  harness ' | cut -d: -f1)
ROW=$(( ROW - 1 ))
echo "(the harness row is screen row $ROW)" >> "$OUT"

sgr "$ROW" 6 M ; sleep 2
shot "1. PRESS ONLY on that row — activation is on RELEASE, so nothing happens"
sgr "$ROW" 6 m ; sleep 4
shot "2. the RELEASE lands — the cursor MOVES to the row; the face stays put"

# THE SECOND STEP: `→` opens the row the click selected.
$S send-keys -t probe Right ; sleep 4
shot "2b. → after the click — NOW it descends (the two-step grammar)"

# A free miss: the footer.
sgr 29 6 M ; sgr 29 6 m ; sleep 2
shot "3. a click on the FOOTER — nothing, and nothing said about it"

# The queue badge, at the column it is actually drawn at.
LINE=$($S capture-pane -p -t probe | head -1)
COL=$(python3 -c "import sys;l=sys.argv[1];print(l.index('▲'))" "$LINE")
echo "(the ▲ badge is at column $COL)" >> "$OUT"
sgr 0 "$COL" M ; sgr 0 "$COL" m ; sleep 4
shot "4. a click on the ▲ queue badge — the queue overlay, same as u"

# A click anywhere inside an overlay dismisses it, as any key already does.
sgr 12 40 M ; sgr 12 40 m ; sleep 2
shot "5. a click inside the overlay dismisses it"

$S send-keys -t probe q ; sleep 1
$S kill-server 2>/dev/null
echo "captured to $OUT"
