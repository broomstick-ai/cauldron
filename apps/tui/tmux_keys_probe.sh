#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  THE ONE QUESTION THIS ROUND HAS TO PUT TO HIM, MEASURED BEFORE IT IS ASKED.
#
#  On his tmux as configured today, `shift-enter` is byte-identical to `enter`
#  and therefore SENDS. No program can tell them apart: tmux's
#  `extended-keys-format` is `xterm`, which answers modifyOtherKeys with
#  `CSI 27;2;13~`, and crossterm has no match arm for a leading 27 — it clears
#  its buffer and the keystroke vanishes. So the face withholds the binding
#  rather than advertising a dead one.
#
#  There is a ONE-LINE change that closes it — `set -s extended-keys-format
#  csi-u` — after which tmux answers `CSI 13;2u`, which crossterm DOES parse
#  and this face already handles. But `claude-code#43169` reports multi-line
#  pastes LOSING THEIR NEWLINES under exactly that setting, and offering him a
#  change that trades one glitch for another would be worse than offering
#  nothing.
#
#  **So this measures both halves before the ticket is written**, on a PRIVATE
#  tmux server (`-L tuikeys -f /dev/null`): his config is neither read nor
#  written, his sessions cannot see it, and the server is killed at the end.
#
#  Usage:  ./tmux_keys_probe.sh
#  Exit:   0 if it could measure; 1 if it could not (never a verdict on him).
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
SOCK="tuikeys"
WORK="$(mktemp -d)"

cleanup() {
    tmux -L "$SOCK" kill-server 2>/dev/null
    rm -rf "$WORK"
}
# The scratch target tree goes with it: bash's traps are last-write-wins, so a
# suite that installs its own EXIT trap would otherwise silently discard the
# one `conformance_target.sh` set and leave half a gigabyte behind.
trap 'cleanup; conformance_target_release' EXIT INT TERM

ok()   { printf '  \033[32m%s\033[0m  %s\n' "OK  " "$1"; }
note() { printf '        %s\n' "$1"; }
head_(){ printf '\n\033[1m%s\033[0m\n' "$1"; }

command -v tmux >/dev/null || { echo "no tmux on PATH; cannot measure"; exit 1; }
(cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -2) >/dev/null

# The synthetic home, exactly as composer_frames.sh uses: one project, one
# thread, so two presses of `→` land in a composer.
FAKEHOME="$WORK/home"
mkdir -p "$FAKEHOME/minds/proof/.mind"
cat > "$FAKEHOME/minds/proof/.mind/threads.json" <<'JSON'
{"threads":[{"id":"00000000-0000-4000-8000-00000000c0de",
             "session_id":"00000000-0000-4000-8000-00000000c0de",
             "summary":"the tmux key probe thread","turn_count":1}]}
JSON

# `run <format> <keys-script>` starts a private tmux server with that
# extended-keys-format, runs the face in a pane, sends the bytes, and prints the
# pane.
run() {
    local fmt="$1" script="$2"
    tmux -L "$SOCK" kill-server 2>/dev/null
    sleep 0.4
    tmux -L "$SOCK" -f /dev/null new-session -d -x 100 -y 24 \
        -e "HOME=$FAKEHOME" \
        -e "TUI_THREADS_CHASSIS_PORT=1" -e "TUI_THREADS_PORT=1" \
        -e "AUI_V2_URL=ws://127.0.0.1:1/" -e "CAULDRON_BIN=/usr/bin/false" \
        -e "CAULDRON_ISOLATED=1" \
        -e "CAULDRON_OPENER=none" -e "CAULDRON_TYPE_ONE=off" \
        -e "CAULDRON_HOME=$WORK/chome" \
        "$BIN" 2>/dev/null || return 1
    tmux -L "$SOCK" set -s extended-keys on 2>/dev/null
    tmux -L "$SOCK" set -s extended-keys-format "$fmt" 2>/dev/null
    sleep 1.5
    # Descend into the composer.
    tmux -L "$SOCK" send-keys Right; sleep 0.6
    tmux -L "$SOCK" send-keys Right; sleep 1.2
    eval "$script"
    sleep 1.0
    tmux -L "$SOCK" capture-pane -p 2>/dev/null
}

# tmux's own literal-byte channel into a pane.
raw() { tmux -L "$SOCK" send-keys -H $(printf '%s' "$1" | xxd -p -c1 | tr '\n' ' '); }

head_ "0. what this tmux says its format is"
tmux -L "$SOCK" -f /dev/null new-session -d "sleep 30" 2>/dev/null
sleep 0.5
DEFAULT_FMT="$(tmux -L "$SOCK" show-options -gv extended-keys-format 2>/dev/null)"
TMUX_VER="$(tmux -V)"
tmux -L "$SOCK" kill-server 2>/dev/null
note "$TMUX_VER, default extended-keys-format: ${DEFAULT_FMT:-<unset>}"
note "his server's live value is read separately and is NOT touched here"

head_ "1. a MULTI-LINE PASTE under extended-keys-format = xterm (his today)"
OUT_X="$(run xterm 'raw "$(printf "\033[200~alpha\nbeta\ngamma\033[201~")"')"
if printf '%s' "$OUT_X" | grep -q "alpha" && printf '%s' "$OUT_X" | grep -q "gamma"; then
    ok "the paste arrived whole"
    printf '%s' "$OUT_X" | grep -n "composer" -A 6 | sed 's/^/        /'
else
    note "the paste did not arrive; pane was:"
    printf '%s' "$OUT_X" | tail -8 | sed 's/^/        /'
fi

head_ "2. the SAME paste under extended-keys-format = csi-u (the proposed change)"
OUT_U="$(run csi-u 'raw "$(printf "\033[200~alpha\nbeta\ngamma\033[201~")"')"
if printf '%s' "$OUT_U" | grep -q "alpha" && printf '%s' "$OUT_U" | grep -q "gamma"; then
    ok "the paste still arrived whole under csi-u"
    printf '%s' "$OUT_U" | grep -n "composer" -A 6 | sed 's/^/        /'
else
    note "THE PASTE WAS CORRUPTED OR LOST under csi-u — this is claude-code#43169"
    printf '%s' "$OUT_U" | tail -8 | sed 's/^/        /'
fi

head_ "3. SHIFT+ENTER under each format — the thing the change would buy"
# `send-keys -N` cannot express shift-enter, so the modifier is asked for the
# way an application does: tmux only forwards extended keys once the app has
# requested them, and the face requests modifyOtherKeys only under csi-u.
for fmt in xterm csi-u; do
    OUT="$(run "$fmt" 'tmux -L "$SOCK" send-keys "before"; sleep 0.4; tmux -L "$SOCK" send-keys -H 1b 5b 32 37 3b 32 3b 31 33 7e; sleep 0.4; tmux -L "$SOCK" send-keys "after"')"
    LINE="$(printf '%s' "$OUT" | grep -n "before" | head -1)"
    if printf '%s' "$OUT" | grep -q "beforeafter"; then
        note "$fmt: CSI 27;2;13~ did NOTHING — the keystroke vanished (crossterm has no arm for 27)"
    elif printf '%s' "$OUT" | grep -q "before"; then
        note "$fmt: CSI 27;2;13~ produced something — $LINE"
    else
        note "$fmt: could not read the pane"
    fi
done

head_ "4. the csi-u spelling, which crossterm CAN read"
for fmt in xterm csi-u; do
    OUT="$(run "$fmt" 'tmux -L "$SOCK" send-keys "before"; sleep 0.4; tmux -L "$SOCK" send-keys -H 1b 5b 31 33 3b 32 75; sleep 0.4; tmux -L "$SOCK" send-keys "after"')"
    if printf '%s' "$OUT" | grep -q "beforeafter"; then
        note "$fmt: CSI 13;2u did nothing"
    else
        note "$fmt: CSI 13;2u made a newline (before/after on separate rows) — the capability works"
    fi
done

printf '\n\033[1mmeasured.\033[0m The verdict belongs in 0042'"'"'s report, not here.\n'
