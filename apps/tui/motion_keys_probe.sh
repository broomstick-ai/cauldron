#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  THE MOTION KEYS, MEASURED RATHER THAN ASSUMED.
#
#  `0052` item 3 says two things that are both measurements and neither of which
#  a key table can answer:
#
#    * `alt-←`/`alt-→` were BUILT in `0042` — VERIFY they work on his tmux
#      (measure the actual bytes tmux emits) rather than trusting the table.
#    * `ctrl-←`/`ctrl-→`/`ctrl-↑`/`ctrl-↓` are to be bound to the LIMIT in that
#      direction *"if it can be made to work in his terminal"*. If tmux swallows
#      them, that is the one legal "cannot" — and it must be MEASURED.
#
#  So this runs `examples/key_probe`, which prints RAW BYTES beside what
#  `vt::Parser` and `input::to_key` make of them, in a pane of a PRIVATE tmux
#  server configured exactly as his live server is:
#
#      extended-keys        on
#      extended-keys-format xterm
#      escape-time          10
#      default-terminal     tmux-256color
#
#  **It cannot touch his tmux.** `-L tuimotion -f /dev/null` is a different
#  SERVER on a different socket, his config is never read or written, and the
#  server is killed on the way out. His live values are read with
#  `show-options`, which writes nothing.
#
#  Usage:  ./motion_keys_probe.sh [output-file]
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

BIN="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/examples/key_probe"
OUT="${1:-$REPO_ROOT/target/tui-motion-keys.txt}"
SOCK="tuimotion"
S="tmux -L $SOCK"

cleanup() { $S kill-server 2>/dev/null; }
# The scratch target tree goes with it: bash's traps are last-write-wins, so a
# suite that installs its own EXIT trap would otherwise silently discard the
# one `conformance_target.sh` set and leave half a gigabyte behind.
trap 'cleanup; conformance_target_release' EXIT INT TERM

command -v tmux >/dev/null || { echo "no tmux on PATH; cannot measure"; exit 1; }
(cd "$REPO_ROOT" && cargo build -p tui --example key_probe 2>&1 | tail -2)
[ -x "$BIN" ] || { echo "key_probe did not build at $BIN"; exit 1; }

: > "$OUT"
{
    echo "tmux: $(tmux -V)"
    echo "his LIVE server (read-only, never written):"
    tmux show-options -s 2>/dev/null | grep -iE "extended-keys|escape-time|default-terminal" | sed 's/^/    /'
    echo
} >> "$OUT"

# `probe <mode> <keys...>` — one private server, one pane, one key list.
probe() {
    local mode="$1"; shift
    $S kill-server 2>/dev/null; sleep 0.3
    local flags=""
    [ "$mode" = enhanced ] && flags="--enhanced"
    $S -f /dev/null new-session -d -x 120 -y 40 "$BIN $flags --seconds 25" 2>/dev/null || return 1
    # His live server's settings, on ours.
    $S set -s extended-keys on 2>/dev/null
    $S set -s extended-keys-format xterm 2>/dev/null
    $S set -s escape-time 10 2>/dev/null
    $S set -s default-terminal tmux-256color 2>/dev/null
    sleep 1.2
    for k in "$@"; do
        $S send-keys "$k" 2>/dev/null
        sleep 0.45
    done
    sleep 0.5
    {
        printf '\n=== %s ===\n' "mode=$mode  keys: $*"
        $S capture-pane -p 2>/dev/null | grep -v '^$'
    } >> "$OUT"
    $S kill-server 2>/dev/null
}

# `M-Up`/`M-Down` are `0100`'s addition and the reason this list grew: the key
# table could not see `opt-↑`/`opt-↓` at all (no `alt` arm for a vertical arrow
# in `input::to_key`), and the rule this crate has been burned by twice is that
# a binding is measured BEFORE it is bound. Expected `CSI 1;3A` / `CSI 1;3B`.
KEYS=(Up Down M-Up M-Down Left Right M-Left M-Right C-Left C-Right C-Up C-Down M-BSpace BSpace M-Delete Delete)
probe plain "${KEYS[@]}"
probe enhanced "${KEYS[@]}"

echo "measured into $OUT"
cat "$OUT"
