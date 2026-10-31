#!/bin/bash
# prove-signal-restore — kill the REAL faces under a pty and read what the pty got (IS-59).
#
#   libs/term-shell/prove-signal-restore.sh <dir holding the built cauldron+tui> [SIG...]
#
# For each face (the cauldron face `tui --cauldron`, the chat face `tui`) and each
# signal (default TERM HUP INT QUIT): run it under script(1) with a scratch HOME and
# CAULDRON_HOME, CAULDRON_OPENER=none, and the chat face pointed at a dead port, so
# nothing of the operator's is read, written or connected to. Wait until it has
# claimed the terminal, send the signal, and require:
#   - exit code 128+signo (script(1) passes the child's through);
#   - after the LAST `CSI > 4 ; 0 m` (modifyOtherKeys off — written only by a
#     restore), the mouse, SGR mouse, bracketed paste and alternate-screen resets
#     are all present and no DECSET (`CSI ? <n> h`) follows.
# Pass KILL to see the check go red: SIGKILL cannot be caught.
set -euo pipefail
BIN=${1:?usage: $0 <bin dir> [SIG...]}; shift
SIGS=("$@"); [ ${#SIGS[@]} -gt 0 ] || SIGS=(TERM HUP INT QUIT)
fail=0; runs=0
num() { kill -l "$1"; }
check() { # <typescript> <rc> <signo> <label>
  python3 - "$@" <<'PY'
import re, sys
out = open(sys.argv[1], 'rb').read(); rc = int(sys.argv[2]); no = int(sys.argv[3]); label = sys.argv[4]
bad = []
if rc != 128 + no: bad.append(f"exit {rc}, want {128 + no}")
i = out.rfind(b"\x1b[>4;0m")
if i < 0: bad.append("no restore at all (modifyOtherKeys-off never written)")
else:
    tail = out[i:]
    for name, seq in [("mouse", b"\x1b[?1002l"), ("sgr", b"\x1b[?1006l"), ("paste", b"\x1b[?2004l"), ("alt-screen", b"\x1b[?1049l")]:
        if seq not in tail: bad.append(f"{name} reset missing after the restore")
    # `?25h` is the cursor coming BACK (HIDE_CURSOR's reset), so it is exempt.
    sets = [m.group(0) for m in re.finditer(rb"\x1b\[\?[0-9;]+h", tail) if m.group(0) != b"\x1b[?25h"]
    if sets: bad.append(f"a DECSET came after the restore: {sets[:3]!r}")
print(("FAIL " if bad else "ok   ") + label + ("" if not bad else ": " + "; ".join(bad)))
sys.exit(1 if bad else 0)
PY
}
for face in cauldron chat; do
  for sig in "${SIGS[@]}"; do
    runs=$((runs + 1))
    S=$(mktemp -d /tmp/term-shell-prove.XXXXXX)
    cmd=("$BIN/tui"); if [ "$face" = cauldron ]; then cmd+=(--cauldron); fi
    # `exec` so the signalled pid IS the face, not a wrapper shell.
    env -i PATH=/usr/bin:/bin HOME="$S" CAULDRON_HOME="$S/c" CAULDRON_OPENER=none \
        TUI_THREADS_PORT=59997 TUI_THREADS_CHASSIS_PORT=59997 TERM=xterm-256color \
        /usr/bin/script -q -F "$S/ts" /bin/sh -c 'echo $$ > "$0/pid"; exec "$@"' "$S" "${cmd[@]}" \
        </dev/null >/dev/null 2>&1 &
    sp=$!
    claimed=0
    for _ in $(seq 1 300); do grep -q $'\e\\[?1049h' "$S/ts" 2>/dev/null && { claimed=1; break; }; perl -e 'select(undef,undef,undef,0.05)'; done
    if [ "$claimed" -ne 1 ]; then
      echo "FAIL $face SIG$sig: the face never claimed the alternate screen"; fail=$((fail + 1))
      kill -KILL "$(cat "$S/pid" 2>/dev/null)" 2>/dev/null || true; wait "$sp" || true; rm -rf "$S"; continue
    fi
    perl -e 'select(undef,undef,undef,0.5)'   # let it draw a few frames
    kill -"$sig" "$(cat "$S/pid")"
    rc=0; wait "$sp" || rc=$?
    check "$S/ts" "$rc" "$(num "$sig")" "$face SIG$sig" || fail=$((fail + 1))
    rm -rf "$S"
  done
done
echo "runs=$runs failed=$fail"
[ "$fail" -eq 0 ]
