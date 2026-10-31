#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  THE POINTER AND MOTION GRAMMAR, PROVEN ON A REAL PTY — `0052`.
#
#  Four things he asked for, and every one of them is a fact only a terminal can
#  answer: which bytes arrive, what the parser makes of them, and what the face
#  does with what the parser made.
#
#    1. A CLICK ON A PLAIN ROW SELECTS IT. It does not drill, does not expand,
#       does not activate the row's default act — and the EXPANDER two columns
#       away still does, because an affordance that stopped working would be the
#       retraction taken one step too far.
#    2. A CLICK IN THE COMPOSER MOVES THE CARET to that position, or the nearest
#       legal one.
#    3. THE MOTION KEYS — `alt-←`/`alt-→` by word (built in `0042`, VERIFIED
#       here rather than trusted), `opt-delete` by word backwards, and
#       `ctrl-←/→/↑/↓` to the limit in each direction.
#    4. A FAST FLICK SCROLLS FAST. A burst of wheel reports moves the distance
#       the burst asked for, in one step.
#
#  `core:writing-tools`: a green you have not forced to fail is not evidence.
#  §5 MUTATES THE SHIPPED SOURCE for each of the four and requires the section
#  that proves it to go red — and it checks that each mutation APPLIED before
#  believing its result, which is `0051`'s own lesson: a mutation that silently
#  did not apply is indistinguishable from a passing test.
#
#  WHAT IT TOUCHES: nothing outside this crate, and NO DAEMON. `pty_drive.py`
#  points the face's endpoint variables at port 1, a port nothing listens on,
#  and refuses to start it at a live port. `$HOME` is
#  redirected to a scratch directory, so the project list, the draft journal and
#  the log are synthetic and his own are neither read nor written. No tmux
#  server of any kind is started, his or ours.
#
#  Usage:  ./pointer_frames.sh
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
DRIVE_PY="$CRATE_DIR/pty_drive.py"
WORK="$(mktemp -d)"
RENDER_RS="$CRATE_DIR/../../libs/tui-render/src/render.rs"
APP_RS="$CRATE_DIR/src/app.rs"
HIT_RS="$CRATE_DIR/../../libs/tui-render/src/hit.rs"
INPUT_RS="$CRATE_DIR/src/input.rs"
# The `?` overlay is rendered FROM the key table, so forcing the overlay check
# to fail means mutating the table — which means backing it up like every other
# source this script touches.
KEYS_RS="$CRATE_DIR/../../libs/tui-render/src/keys.rs"
BACKUP="$WORK/backup"
FAILURES=0

mkdir -p "$BACKUP"
cp "$APP_RS" "$BACKUP/app.rs"
cp "$RENDER_RS" "$BACKUP/render.rs"
cp "$HIT_RS" "$BACKUP/hit.rs"
cp "$INPUT_RS" "$BACKUP/input.rs"
cp "$KEYS_RS" "$BACKUP/keys.rs"
restore() {
    cp "$BACKUP/app.rs" "$APP_RS"
    cp "$BACKUP/render.rs" "$RENDER_RS"
    cp "$BACKUP/hit.rs" "$HIT_RS"
    cp "$BACKUP/input.rs" "$INPUT_RS"
    cp "$BACKUP/keys.rs" "$KEYS_RS"
}
FAKE_PID=""
cleanup() {
    restore
    [ -n "$FAKE_PID" ] && kill "$FAKE_PID" 2>/dev/null
    rm -rf "$WORK"
}
# The scratch target tree goes with it: bash's traps are last-write-wins, so a
# suite that installs its own EXIT trap would otherwise silently discard the
# one `conformance_target.sh` set and leave half a gigabyte behind.
trap 'cleanup; conformance_target_release' EXIT INT TERM

ok()    { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
bad()   { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
note()  { printf '        %s\n' "$1"; }
head_() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# ── A ROW THIS SUITE CANNOT FIND IS AN ABORT, NOT A `-1` ─────────────────────
# **MEASURED 2026-09-28, and it is the reason five checks in this file once
# reported on the mouse when the mouse was never touched.** Every click below
# is aimed at a row number this suite reads off a captured screen:
#
#     TARGET_ROW="$(… | grep -n 'proof6' | head -1 | cut -d: -f1)"
#     TARGET_ROW=$(( TARGET_ROW - 1 ))
#
# When the grep finds nothing that arithmetic is `$(( "" - 1 ))`, which bash
# evaluates to **-1** without a word of complaint. `click -1 10` then puts `0`
# on the wire, SGR mouse rows are 1-based, and the face correctly ignores it —
# so the suite printed *"the click did not move the cursor (cursor on 5,
# clicked -1)"* and three more like it. Four accusations against the click
# path, produced by a screen that never painted.
#
# An unfindable anchor is therefore a HARD STOP here, not a value: everything
# after it would be measuring a click that was never aimed at anything, and
# `0065`'s rule is that an absence is never evidence. `0046`'s anchor shape —
# an explicit `ANCHOR-MISSING <label>` beside a `FAIL` — is what it prints.
row_of() { # <screen-text> <needle> <label>  ->  0-based row, or exit 1
    local n
    n="$(printf '%s\n' "$1" | grep -n -- "$2" | head -1 | cut -d: -f1)"
    [ -n "$n" ] || return 1
    printf '%d' $(( n - 1 ))
}
abort_fixture() { # <label> <screen-text> <why>
    printf '  \033[31mANCHOR-MISSING\033[0m %s\n' "$1"
    bad "$3"
    printf '%s\n' "$2" | sed -n '1,20p' | sed 's/^/        /'
    printf '\n\033[1mresult\033[0m\n'
    printf '  \033[31mABORTED\033[0m — the fixture never came up, so nothing below it\n'
    printf '  could measure anything. This is NOT a finding about the pointer.\n'
    exit 1
}

build() { (cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -3); }

# ── the synthetic home ───────────────────────────────────────────────────────
# SIX projects, so the root menu is a LIST a wheel can move a cursor through,
# and one thread inside the first so `→ →` still lands in a composer.
FAKEHOME="$WORK/home"
for i in 1 2 3 4 5 6; do
    mkdir -p "$FAKEHOME/minds/proof$i/.mind"
done
cat > "$FAKEHOME/minds/proof1/.mind/threads.json" <<'JSON'
{"threads":[{"id":"00000000-0000-4000-8000-00000000c0de",
             "session_id":"00000000-0000-4000-8000-00000000c0de",
             "summary":"the pointer proof thread",
             "last_turn_at":"2026-09-14T12:00:00Z","turn_count":1}]}
JSON

drive()  { python3 "$DRIVE_PY" "${2:-100}" "${3:-24}" "$BIN" "$FAKEHOME" "$1"; }
screen() { printf '%s' "$1" | sed -n 's/^SCREEN //p'; }
# **THE HEADER LINE ONLY.** Grepping the whole screen for "projects" finds the
# FOOTER too — `← projects` is the List mode back-binding's short label — which
# is a false positive that made a forced failure look like a passing check.
header() { screen "$1" | sed -n '1p'; }
# **RE-KEYED IN `0072`, OFF THE HEADER AND ONTO THE BODY.** This read
# `header "$1" | grep -q '^projects ·'`, which was the root header's TITLE. The
# operator cut that header to three things — the unread count, an active count
# and `connected` — so the word `projects` left row 0 and every check in this
# file that gates on `at_root` went red at once.
#
# It now keys on the `projects` BAND HEADER, which is where the word lives and
# where he moved its count to ("You can say the number of projects available in
# the band header"). That is a STRONGER test than the old one: the title is
# chrome, and this proves the root's SURFACE drew.
#
# The old comment's footer trap still applies and is still avoided — `←
# projects` has no `· <digits>` after it, and a band header always does.
# `[▸ ]` because a band header is a cursor stop since `DEC-46` and may be
# wearing the cursor marker rather than the gutter.
at_root() { screen "$1" | grep -qE '^[▸ ] *projects · [0-9]'; }
has()    { printf '%s' "$1" | grep -qF -- "$2"; }

# **`↓ → →` REACHES THE COMPOSER — and the `↓` is new in `0072`.**
#
# It was `→ →`. The root's first cursor stop used to be the first RECENT row,
# so one `→` opened that thread outright. Since `DEC-46` the first stop is the
# recent band's own HEADER, where `→` EXPANDS the band instead of descending —
# so the old spelling pressed `→` twice against a header and never left the
# root. One `↓` steps onto the row, and the descent is unchanged after that.
#
# This is the fixture catching up with the grammar, not a workaround: the
# operator asked for header-as-cursor-target, and a driver that assumed row 0
# was content was encoding the assumption that changed.
DESCEND='(b"\x1b[B", 0.8), (b"\x1b[C", 0.6), (b"\x1b[C", 1.2),'
into()   { drive "[$DESCEND $1]" "${2:-100}" "${3:-24}"; }

# SGR 1006 mouse reports, 1-based on the wire. `m` is a release.
# `press ROW COL` / `release ROW COL` produce the literal bytes.
sgr()     { printf '(b"\\x1b[<%d;%d;%d%s", %s),' "$1" "$(( $3 + 1 ))" "$(( $2 + 1 ))" "$4" "$5"; }
click()   { printf '%s%s' "$(sgr 0 "$1" "$2" M 0.2)" "$(sgr 0 "$1" "$2" m "${3:-1.2}")"; }
# A wheel notch is button 64 (up) / 65 (down), press-form only.
wheel()   { printf '(b"\\x1b[<%d;10;10M", %s),' "$1" "${2:-0.0}"; }
wheelrun(){ local n=$1 btn=$2 out=""; for _ in $(seq "$n"); do out="$out$(wheel "$btn" 0.0)"; done; printf '%s' "$out"; }

build >/dev/null
[ -x "$BIN" ] || { echo "the face did not build"; exit 1; }

# ─────────────────────────────────────────────────────────────────────────────
head_ "0. the fixture — six projects at the root, a composer two → away"
# ─────────────────────────────────────────────────────────────────────────────
ROOT="$(drive '[]')"
if has "$ROOT" "proof1" && has "$ROOT" "proof6"; then
    ok "the root menu lists six projects"
else
    bad "the fixture is wrong, not the code"
    screen "$ROOT" | sed -n '1,12p' | sed 's/^/        /'
fi
COMP="$(into '')"
# **ANCHORED ON THE BOX, NOT ON THE BAND'S LEGEND.** This asked for the words
# `composer` and `enter SENDS all`, which were the band'"'"'s permanent teaching row
# until the operator said he did not need instructions on how to use a textarea
# and they were removed. An anchor on a LABEL goes stale in silence the moment
# the label is a design decision; an empty box's own placeholder does not.
if has "$COMP" "type a message"; then
    ok "the descent reaches a real composer"
else
    bad "never reached the composer"
    screen "$COMP" | sed -n '1,26p' | sed 's/^/        /'
fi

# ─────────────────────────────────────────────────────────────────────────────
head_ "1. A CLICK ON A PLAIN ROW SELECTS IT — it does not drill"
# ─────────────────────────────────────────────────────────────────────────────
# The root's cursor is drawn as `▸ ` on the selected row. Clicking the LAST
# project must move that marker there and leave the face on the root.
ROWS_OUT="$(screen "$ROOT")"
TARGET_ROW="$(row_of "$ROWS_OUT" 'proof6' 'proof6-row')" || abort_fixture \
    'proof6-row' "$ROWS_OUT" \
    'the root screen has no proof6 row — every click below would be aimed at row -1'
note "proof6 is on screen row $TARGET_ROW"

CLICKED="$(drive "[$(click "$TARGET_ROW" 10 2.0)]")"
if at_root "$CLICKED"; then
    ok "a click did NOT drill — the header still says the root, as he ruled"
else
    bad "a click DRILLED IN: the retraction is not in force"
    screen "$CLICKED" | sed -n '1,3p' | sed 's/^/        /'
fi
CURSOR_LINE="$(screen "$CLICKED" | grep -n '^▸' | head -1 | cut -d: -f1)"
CURSOR_LINE=$(( ${CURSOR_LINE:-0} - 1 ))
if [ "$CURSOR_LINE" = "$TARGET_ROW" ]; then
    ok "the click MOVED THE CURSOR onto the row it landed on (row $TARGET_ROW)"
else
    bad "the click did not move the cursor (cursor on $CURSOR_LINE, clicked $TARGET_ROW)"
    screen "$CLICKED" | sed -n '3,12p' | sed 's/^/        /'
fi

# THE SECOND STEP: `→` after the click is what opens what he picked.
TWO_STEP="$(drive "[$(click "$TARGET_ROW" 10 1.0) (b\"\\x1b[C\", 2.0),]")"
if has "$TWO_STEP" "proof6"; then
    ok "→ after the click descended into the row the click selected"
else
    bad "→ after a click did not descend"
    screen "$TWO_STEP" | sed -n '1,3p' | sed 's/^/        /'
fi

# ─────────────────────────────────────────────────────────────────────────────
head_ "1b. …AND THE EXPANDER TWO COLUMNS AWAY STILL ACTIVATES"
# ─────────────────────────────────────────────────────────────────────────────
# *"Buttons and explicit affordances (the queue count, sheet action rows, play
# buttons, expanders) still activate on click."* — so the retraction has a line
# in it, and this is the section that finds the line.
#
# A transcript with something IN it needs a daemon, so this one uses the
# crate's own `examples/fake_daemon` on a loopback port. It is not the real
# daemon and cannot reach one: `examples/fake_daemon.rs` binds 127.0.0.1 and
# answers out of memory; `pty_drive.py` hands the face its PORT through the
# variable the seam reads, and refuses a live one (3653 is his threads).
# **A HARD-CODED PORT THAT IS ALREADY IN USE MUST ABORT, NOT PRODUCE AN EMPTY
# SCREEN** (`0065`). This used to fire the fake daemon at a fixed port, sleep,
# and carry on regardless. If the bind failed — another run of this suite, a
# stale daemon, anything at all on 18791 — the face connected to whatever WAS
# there or to nothing, the transcript came up empty, and the section below
# scored a PASS on the strength of a string being absent from a blank screen.
# The port is now checked before the launch and the LISTEN is waited for and
# verified after it; either failure is fatal and says exactly what to do.
FAKE_PORT="${TUI_FAKE_PORT:-18791}"
port_in_use() { python3 -c 'import socket,sys
s = socket.socket()
try:
    s.bind(("127.0.0.1", int(sys.argv[1])))
except OSError:
    sys.exit(0)
finally:
    s.close()
sys.exit(1)' "$1"; }
port_listening() { python3 -c 'import socket,sys
s = socket.socket(); s.settimeout(0.3)
sys.exit(0 if s.connect_ex(("127.0.0.1", int(sys.argv[1]))) == 0 else 1)' "$1"; }
die() { printf '\n\033[31mABORTING\033[0m — %s\n' "$1"; shift; for l in "$@"; do printf '  %s\n' "$l"; done; exit 1; }

if port_in_use "$FAKE_PORT"; then
    die "port $FAKE_PORT is ALREADY IN USE, so the fake daemon cannot bind it." \
        "Something else is on 127.0.0.1:$FAKE_PORT — most likely a fake_daemon" \
        "left behind by an interrupted run of this suite." \
        "This is fatal ON PURPOSE: without the fake daemon the transcript comes" \
        "up EMPTY, and a section that asserts a string is absent would score a" \
        "PASS on a blank screen. Kill the holder, or set TUI_FAKE_PORT to a" \
        "free port and re-run."
fi
(cd "$REPO_ROOT" && cargo build -p tui --example fake_daemon 2>&1 | tail -2) >/dev/null
"${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/examples/fake_daemon" "$FAKE_PORT" >"$WORK/fake.log" 2>&1 &
FAKE_PID=$!
BOUND=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
    if ! kill -0 "$FAKE_PID" 2>/dev/null; then break; fi
    if port_listening "$FAKE_PORT"; then BOUND=1; break; fi
    sleep 0.5
done
if [ "$BOUND" -ne 1 ]; then
    printf '  fake_daemon log:\n'; sed 's/^/    /' "$WORK/fake.log" 2>/dev/null | head -20
    die "the fake daemon never started listening on 127.0.0.1:$FAKE_PORT." \
        "Its log is above. Every check in 1b and in 5(e) reads a screen that" \
        "would be EMPTY, so this stops here rather than passing on absence."
fi
note "fake daemon listening on 127.0.0.1:$FAKE_PORT (pid $FAKE_PID)"

FAKEHOME2="$WORK/home2"
mkdir -p "$FAKEHOME2/minds/proof1/.mind"
cat > "$FAKEHOME2/minds/proof1/.mind/threads.json" <<'JSON'
{"threads":[{"id":"t","session_id":"t","summary":"the expander proof thread","turn_count":1}]}
JSON
live() { python3 "$DRIVE_PY" 100 24 "$BIN" "$FAKEHOME2" "$1" "ws://127.0.0.1:$FAKE_PORT/ws"; }
# The fake daemon's history carries a `tool_use` with no result, which `0051`
# renders as a merged tool row that is EXPANDABLE — so the face draws an
# expander on it and this is a real one rather than a fixture's idea of one.
# **`↓ → →`, and the `↓` is new in `0072`** — the same reason `DESCEND` grew
# one: the root's first cursor stop is a band HEADER now, where `→` expands the
# band instead of descending, so a driver that pressed `→` twice from row 0
# never left the root and every check below it read an empty transcript.
TOOL_ROW='(b"\x1b[B",0.8),(b"\x1b[C",0.6),(b"\x1b[C",1.8),'

BASE="$(live "[$TOOL_ROW]")"
TOOL_LINE="$(screen "$BASE" | grep -n '⚙ tool' | head -1 | cut -d: -f1)"
TOOL_LINE=$(( ${TOOL_LINE:-0} - 1 ))
# **THE MARK'S COLUMN IS READ OFF THE SCREEN, NOT ASSUMED** (`0117`). It used
# to be column zero, because every transcript row began with exactly the
# two-column gutter. The event lane became the CENTRED INFO BLOB — *"info blobs
# should be centered with a max character size that enforces a margin on each
# side"* — so the mark now sits immediately left of the blob, wherever that is
# at this width. A probe that kept clicking column zero would not have failed
# loudly; it would have clicked the MARGIN and reported that the expander does
# not work.
EXP_COL="$(screen "$BASE" | python3 -c '
import sys
for line in sys.stdin:
    i = line.find("\u25b8 \u2699 tool")
    if i >= 0:
        print(i)
        break
else:
    print(-1)
')"
if [ "$TOOL_LINE" -ge 0 ] && [ "${EXP_COL:--1}" -ge 0 ]; then
    ok "an expandable row DRAWS its expander beside the blob (screen row $TOOL_LINE, column $EXP_COL)"
else
    bad "no expander is drawn — an affordance he cannot see is one he cannot click"
    screen "$BASE" | sed -n '12,20p' | sed 's/^/        /'
    EXP_COL=0
fi

OPENED="$(live "[$TOOL_ROW $(click "$TOOL_LINE" "$EXP_COL" 1.5)]")"
# Two columns right of the mark — the blob's GLYPH column, which is body and
# not button. Derived from the mark for the same reason the mark is derived
# from the screen: at a different width it is a different column.
BODYCLICK="$(live "[$TOOL_ROW $(click "$TOOL_LINE" "$(( EXP_COL + 2 ))" 1.5)]")"
if screen "$OPENED" | grep -q 'file_path'; then
    ok "a click ON THE EXPANDER opened the row — the raw payload is on screen"
else
    bad "a click on the expander did not open the row"
    screen "$OPENED" | grep -n 'tool ·' | sed 's/^/        /'
fi
# **ABSENCE OF A STRING IS NOT EVIDENCE OF THE BEHAVIOUR** (`0065`). This check
# used to be nothing but "`file_path` is not on the screen", and that is equally
# what you get from a fake daemon that never bound, a `→ →` that never reached
# the tool row, or a face that died on startup — the same mistake as reading a
# search that found nothing as an answer about the world. So the screen is
# asserted POSITIVELY FIRST: the row the click landed on has to BE there, drawn
# and unexpanded, before its unexpanded-ness can mean anything at all.
BODY_TOOL_ROWS="$(screen "$BODYCLICK" | grep -c '⚙ tool')"
if [ "$BODY_TOOL_ROWS" -eq 0 ]; then
    bad "THE BODY-CLICK SCREEN HAS NO TOOL ROW ON IT — this section measured nothing. An empty or crashed screen is not a retraction."
    screen "$BODYCLICK" | sed -n '1,20p' | sed 's/^/        /'
elif screen "$BODYCLICK" | grep -q 'file_path'; then
    bad "a click on the row's BODY expanded it — the retraction is not in force"
else
    ok "a click on the row's BODY two columns away did NOTHING, as he ruled"
    note "and the row IS on the screen it did nothing to: $BODY_TOOL_ROWS tool row(s) drawn"
    note "expander: $(screen "$OPENED" | grep -o 'tool · Read · .\{0,34\}' | head -1)"
    note "body:     $(screen "$BODYCLICK" | grep -o 'tool · Read · .\{0,34\}' | head -1)"
fi

# ─────────────────────────────────────────────────────────────────────────────
head_ "2. A CLICK IN THE COMPOSER MOVES THE CARET"
# ─────────────────────────────────────────────────────────────────────────────
# Type a word, click back into the middle of it, and type a marker. Where the
# marker lands IS the caret's position, read off the screen rather than asked
# for — there is no other way to see a caret that is painted as a reversed cell.
TEXT='(b"abcdefgh", 0.8),'
COMPOSER_ROW="$(row_of "$(screen "$COMP")" 'type a message' 'composer-row')" || abort_fixture \
    'composer-row' "$(screen "$COMP")" \
    'the composer never drew — every caret click below would be aimed at row -1'
note "the composer's first text row is screen row $COMPOSER_ROW"

# Column 2 is the band's gutter, so `gutter + 3` is between `c` and `d`.
CARET="$(into "$TEXT $(click "$COMPOSER_ROW" 5 0.6) (b\"Z\", 0.8),")"
if screen "$CARET" | grep -q "abcZdefgh"; then
    ok "a click put the caret where he pointed: 'abcZdefgh'"
else
    bad "the caret did not land at the clicked column"
    screen "$CARET" | grep -n "abc" | sed 's/^/        /'
fi

# PAST THE END OF THE TEXT is the end of the text — "the nearest legal one".
FAR="$(into "$TEXT $(click "$COMPOSER_ROW" 80 0.6) (b\"Z\", 0.8),")"
if screen "$FAR" | grep -q "abcdefghZ"; then
    ok "a click past the last character landed AFTER it — the nearest legal one"
else
    bad "a click past the end of the text did not clamp to the end"
    screen "$FAR" | grep -n "abcdefgh" | sed 's/^/        /'
fi

# ─────────────────────────────────────────────────────────────────────────────
head_ "3. THE MOTION KEYS, ON THE BYTES HIS TMUX ACTUALLY EMITS"
# ─────────────────────────────────────────────────────────────────────────────
# Every byte string below was MEASURED by `motion_keys_probe.sh` on a private
# tmux 3.5a server configured exactly as his live one. They are not guesses at
# an encoding; they are a recording of one.

# alt-← by word: `CSI 1;3D`. Two of them from the end of "alpha beta gamma"
# put the caret at the start of "beta", where a marker must appear.
WORD="$(into '(b"alpha beta gamma", 0.8), (b"\x1b[1;3D", 0.3), (b"\x1b[1;3D", 0.3), (b"Z", 0.8),')"
if screen "$WORD" | grep -q "alpha Zbeta gamma"; then
    ok "alt-← moves BY WORD on the bytes tmux emits (CSI 1;3D) — 0042 verified"
else
    bad "alt-← did not move by word"
    screen "$WORD" | grep -n "alpha" | sed 's/^/        /'
fi

# alt-→ back over one word.
WORDR="$(into '(b"alpha beta gamma", 0.8), (b"\x1b[1;3D", 0.3), (b"\x1b[1;3D", 0.3), (b"\x1b[1;3C", 0.3), (b"Z", 0.8),')"
if screen "$WORDR" | grep -q "alpha betaZ gamma"; then
    ok "alt-→ moves by word the other way (CSI 1;3C)"
else
    bad "alt-→ did not move by word"
    screen "$WORDR" | grep -n "alpha" | sed 's/^/        /'
fi

# opt-delete: a word backwards. BOTH dialects tmux emits, because which one
# arrives depends on whether modifyOtherKeys was negotiated.
for spelling in '\x1b\x7f:ESC-DEL' '\x1b[27;3;127~:CSI-27-3-127'; do
    BYTES="${spelling%%:*}"; NAME="${spelling##*:}"
    DEL="$(into "(b\"alpha beta gamma\", 0.8), (b\"$BYTES\", 0.8),")"
    # NOT `"alpha beta "` — the driver rstrips every row, so a trailing space
    # can never appear on screen and the assertion could never have passed.
    #
    # **AND NOT MERELY "gamma IS GONE"** (`0065`). A PLAIN backspace deletes one
    # character and leaves `alpha beta gamm`, which contains "alpha beta" and
    # does not contain "gamma" — so the old pair of greps passed the exact
    # regression 5h forces, and this assertion could not tell a word delete from
    # a character delete. The bar is "alpha beta AND NOTHING AFTER IT".
    if screen "$DEL" | grep -q "alpha beta" && ! screen "$DEL" | grep -q "alpha beta g"; then
        ok "opt-delete ($NAME) cut the word before the caret"
    else
        bad "opt-delete ($NAME) did not cut a word"
        screen "$DEL" | grep -n "alpha" | sed 's/^/        /'
    fi
done

# ctrl-← / ctrl-→ : the limits of the LINE.
LIM="$(into '(b"alpha beta gamma", 0.8), (b"\x1b[1;5D", 0.4), (b"Z", 0.8),')"
if screen "$LIM" | grep -q "Zalpha beta gamma"; then
    ok "ctrl-← jumped to the start of the line (CSI 1;5D)"
else
    bad "ctrl-← did not reach the line start — this is the key that was being FLATTENED to a plain ←"
    screen "$LIM" | grep -n "alpha" | sed 's/^/        /'
fi
LIMR="$(into '(b"alpha beta gamma", 0.8), (b"\x1b[1;5D", 0.4), (b"\x1b[1;5C", 0.4), (b"Z", 0.8),')"
if screen "$LIMR" | grep -q "alpha beta gammaZ"; then
    ok "ctrl-→ jumped to the end of the line (CSI 1;5C)"
else
    bad "ctrl-→ did not reach the line end"
    screen "$LIMR" | grep -n "alpha" | sed 's/^/        /'
fi

# ctrl-↑ / ctrl-↓ : the limits of the whole BUFFER, across logical lines.
TOP="$(into '(b"one", 0.4), (b"\x1b\r", 0.3), (b"two", 0.6), (b"\x1b[1;5A", 0.4), (b"Z", 0.8),')"
if screen "$TOP" | grep -q "Zone"; then
    ok "ctrl-↑ jumped to the TOP of the draft, across a newline (CSI 1;5A)"
else
    bad "ctrl-↑ did not reach the top of the draft"
    screen "$TOP" | grep -n "one\|two" | sed 's/^/        /'
fi
BOT="$(into '(b"one", 0.4), (b"\x1b\r", 0.3), (b"two", 0.6), (b"\x1b[1;5A", 0.4), (b"\x1b[1;5B", 0.4), (b"Z", 0.8),')"
if screen "$BOT" | grep -q "twoZ"; then
    ok "ctrl-↓ jumped to the END of the draft (CSI 1;5B)"
else
    bad "ctrl-↓ did not reach the end of the draft"
    screen "$BOT" | grep -n "one\|two" | sed 's/^/        /'
fi

# The `?` overlay is rendered FROM the key table, so the new keys are in it for
# free — and this is the check that "for free" actually happened.
# **RE-KEYED IN ROUND SIX: THE OVERLAY'S DOOR MOVED.** Every stimulus in this
# file used to send ESC followed by a slash — the `opt-/` spelling — which
# `0134` RETIRED when every `opt-<letter>`/`<punct>`
# became `ctrl-*`. Help is now `?` / `^/` / `^7`, and on his stack the bare byte
# is `0x1f` (`vt.rs:406` maps it to `Ctrl('7')`). With the old spelling the
# overlay simply never opened, so this check and the four after it failed
# against a screen that was never raised — five reds, one stale keystroke, and
# ZERO of them a defect in the face.
#
# The INPUT moved; not one assertion was weakened. That is the rule: when a
# contract moves, re-key the probe's stimulus and leave its verdict alone.
#
# **AND THIS NOTE DELIBERATELY DESCRIBES THE OLD BYTE RATHER THAN SPELLING IT.**
# The manager's first draft quoted it literally, and the very sweep that
# replaced the old stimuli rewrote the quotation inside this comment too,
# leaving a note that contradicted itself. Same lesson as an anchor: describe
# the thing you are replacing; do not reproduce it in the file being edited.
KEYS="$(into '(b"\x1f", 1.2),' 100 44)"
MISSING=""
# The modifier descriptor is chosen from the HOST — `opt-` on a Mac, `alt-`
# elsewhere — so a probe that hard-codes one spelling tests the machine it was
# written on. `MOD` is read off the face's own overlay rather than guessed.
MOD="alt-"; has "$KEYS" "opt-" && MOD="opt-"
for k in "^←" "^→" "^↑" "^↓" "${MOD}bksp"; do
    has "$KEYS" "$k" || MISSING="$MISSING $k"
done
if [ -z "$MISSING" ]; then
    ok "the ? overlay prints every new key — one table, two readers"
else
    bad "the overlay does not mention:$MISSING"
fi

# ─────────────────────────────────────────────────────────────────────────────
head_ "4. A FAST FLICK SCROLLS FAST"
# ─────────────────────────────────────────────────────────────────────────────
# The `?` overlay in a thread is forty-odd bindings — longer than the window and
# scrollable, which makes it the one surface with real depth that needs NO
# daemon. A burst of wheel reports over it must travel the distance the burst
# asked for, and must do it in ONE frame rather than creeping.
FIRST_LINE() { screen "$1" | sed -n '4p'; }

OVERLAY="$(into '(b"\x1f", 1.2),')"
BEFORE="$(FIRST_LINE "$OVERLAY")"
# ONE notch, then a burst of eight, written as ONE chunk and given a SHORT
# settle — if the face were spending them one at a time it would still be
# working when the screen is read.
ONE="$(into "(b\"\\x1f\", 1.2), $(wheelrun 1 65) (b\"\", 0.35),")"
MANY="$(into "(b\"\\x1f\", 1.2), $(wheelrun 8 65) (b\"\", 0.35),")"
AFTER_ONE="$(FIRST_LINE "$ONE")"
AFTER_MANY="$(FIRST_LINE "$MANY")"
note "before:     ${BEFORE:0:60}"
note "one notch:  ${AFTER_ONE:0:60}"
note "eight:      ${AFTER_MANY:0:60}"

if [ "$AFTER_ONE" != "$BEFORE" ]; then
    ok "one wheel notch moved the overlay"
else
    bad "a wheel notch over the overlay did nothing at all"
fi
if [ "$AFTER_MANY" != "$AFTER_ONE" ] && [ "$AFTER_MANY" != "$BEFORE" ]; then
    ok "EIGHT notches went eight times as far, in one frame's settle — not one row at a time"
else
    bad "a burst of eight landed in the same place as one — the burst is being spent one notch at a time"
fi

# And the distance itself: the overlay scrolled by 8 * WHEEL_ROWS rows, which
# means the row that was 40 down is now at the top of the body.
DEPTH="$(screen "$OVERLAY" | sed -n '4,20p' | wc -l | tr -d ' ')"
note "the overlay body is $DEPTH rows tall in this window"

# ─────────────────────────────────────────────────────────────────────────────
head_ "5. FORCED — mutate each mechanism out and watch its section go red"
# ─────────────────────────────────────────────────────────────────────────────
# `0051`'s lesson, applied: a mutation that silently did NOT apply is
# indistinguishable from a passing test, so every one of these asserts its
# anchor was found before it believes the result.
mutate() { # file, old, new, label
    python3 - "$1" "$2" "$3" "$4" <<'MUT'
import sys
path, old, new, label = sys.argv[1:5]
s = open(path).read()
if old not in s:
    print(f"ANCHOR-MISSING {label}")
    sys.exit(3)
open(path, "w").write(s.replace(old, new, 1))
print(f"APPLIED {label}")
MUT
}

# (a) the retraction itself: point the click arm back at Open.
#
# ANCHOR REPAIRED (0063). This went ANCHOR-MISSING because `0062`'s successor
# added `| Mode::NewProject` to the click arm — correctly: `Mode::NewProject` is
# the ROOT with a live field in one of its rows, so the root's own click grammar
# must still hold under him. The GUARD was right and the CODE was the change, so
# the anchor is RESTATED to the arm as it now reads rather than loosened into a
# pattern that would match anything. The property is unchanged and is the whole
# point of this check: a click on a plain row SELECTS, and pointing that arm
# back at `Open` must make the face drill in again.
M="$(mutate "$HIT_RS" \
    "(Target::RootRow(i), Mode::Projects | Mode::NewProject, Via::Click)
        | (Target::ThreadRow(i), Mode::List, Via::Click) => Some(Intent::Select(i))," \
    "(Target::RootRow(_), Mode::Projects | Mode::NewProject, Via::Click)
        | (Target::ThreadRow(_), Mode::List, Via::Click) => Some(Intent::Open)," \
    "click-selects")"
case "$M" in
    APPLIED*)
        build >/dev/null
        # **ONE `↓` BEFORE THE CLICK, and it is what keeps this forced failure
        # DISCRIMINATING** (`0072`). With the arm mutated to `Open`, a click no
        # longer moves the cursor — `Intent::Open` acts on wherever the cursor
        # already IS. Since `DEC-46` that is the recent band's HEADER at row 0,
        # where `Open` expands the band and the face correctly stays on the
        # root — so the mutant looked identical to the real thing and this
        # check reported "not discriminating" about itself rather than about
        # the code. The `↓` puts the cursor on a content row, where `Open` has
        # somewhere to go, and the mutation is visible again.
        MUT_OUT="$(drive "[(b\"\\x1b[B\",0.4),$(click "$TARGET_ROW" 10 2.0)]")"
        if at_root "$MUT_OUT"; then
            bad "click-selects is NOT discriminating: the face stayed on the root with the arm mutated to Open"
        else
            ok "click-selects forced to fail — with the old arm the click drills in again"
            note "and it opened the CURSOR's row rather than the clicked one: $(header "$MUT_OUT" | head -c 40)"
        fi ;;
    *) bad "click-selects mutation did not apply: $M" ;;
esac
restore; build >/dev/null

# (b) the caret: throw the clicked column away.
M="$(mutate "$APP_RS" \
    "let col = (col as usize).saturating_sub(tui_render::GUTTER);" \
    "let col = 0; let _ = col;" \
    "caret-column")"
case "$M" in
    APPLIED*)
        build >/dev/null
        MUT_OUT="$(into "$TEXT $(click "$COMPOSER_ROW" 5 0.6) (b\"Z\", 0.8),")"
        if screen "$MUT_OUT" | grep -q "abcZdefgh"; then
            bad "the caret check is NOT discriminating: it passed with the column discarded"
        else
            ok "the caret forced to fail — with the column discarded it lands at column 0"
        fi ;;
    *) bad "caret mutation did not apply: $M" ;;
esac
restore; build >/dev/null

# (c) the limit keys: put back the flattening that was there before `0052`.
M="$(mutate "$INPUT_RS" \
    "KeyCode::Left if ctrl => Key::CtrlLeft," \
    "KeyCode::Left if ctrl && false => Key::CtrlLeft," \
    "ctrl-arrow")"
case "$M" in
    APPLIED*)
        build >/dev/null
        MUT_OUT="$(into '(b"alpha beta gamma", 0.8), (b"\x1b[1;5D", 0.4), (b"Z", 0.8),')"
        if screen "$MUT_OUT" | grep -q "Zalpha beta gamma"; then
            bad "the ctrl-← check is NOT discriminating: it passed with the arm removed"
        else
            ok "ctrl-← forced to fail — without the arm it is flattened to a plain ← again"
            note "which is EXACTLY how it shipped: the bytes arrived, the parser read them, and this line dropped the modifier"
        fi ;;
    *) bad "ctrl-arrow mutation did not apply: $M" ;;
esac
restore; build >/dev/null

# (d) the wheel: spend a run one notch at a time, the way it shipped.
# The anchor moved when the run gained a MEASURED gain: it was
# `n.saturating_mul(WHEEL_ROWS)` and it is `rows` now. The mutation is the same
# defect either way — spend the run one notch at a time, the way it shipped.
M="$(mutate "$APP_RS" \
    "self.scroll_run(up, rows);" \
    "for _ in 0..n { self.scroll_run(up, 1); }" \
    "wheel-run")"
case "$M" in
    APPLIED*)
        build >/dev/null
        MUT_OUT="$(into "(b\"\\x1f\", 1.2), $(wheelrun 8 65) (b\"\", 0.35),")"
        if [ "$(FIRST_LINE "$MUT_OUT")" = "$AFTER_MANY" ]; then
            bad "the burst check is NOT discriminating: eight notches went as far with the run spent one at a time"
        else
            ok "the burst forced to fail — spent one notch at a time it travels a fifth as far"
            MUT_LINE="$(FIRST_LINE "$MUT_OUT")"
            note "one-at-a-time: ${MUT_LINE:0:50}"
        fi ;;
    *) bad "wheel-run mutation did not apply: $M" ;;
esac
restore; build >/dev/null

# (e) the expander: draw the glyph but claim no columns, so the button is
# painted and the mouse falls straight through it.
M="$(mutate "$RENDER_RS" \
    "info.spans.push((at, at + GUTTER as u16, Target::Expander(i)));" \
    "let _ = (at, i);" \
    "expander-claim")"
case "$M" in
    APPLIED*)
        build >/dev/null
        MUT_OUT="$(live "[$TOOL_ROW $(click "$TOOL_LINE" 0 1.5)]")"
        if screen "$MUT_OUT" | grep -q 'file_path'; then
            bad "the expander check is NOT discriminating: the click opened the row with the claim removed"
        else
            ok "the expander forced to fail — unclaimed, the glyph is painted and the click falls through it"
        fi ;;
    *) bad "expander mutation did not apply: $M" ;;
esac
restore; build >/dev/null


# ─────────────────────────────────────────────────────────────────────────────
head_ "5f. FORCED — the motion keys of section 3, one arm at a time"
# ─────────────────────────────────────────────────────────────────────────────
# `0065`: section 3 asserts SEVEN motion behaviours plus the `?` overlay's key
# list, and exactly ONE of them — `ctrl-←`, forced in (c) above — had ever been
# watched bite. The other six and the overlay had never been forced at all, so
# nobody knew whether any of them could tell. Each mutation below disarms the
# guard on one arm, so the key falls through to the UNMODIFIED arm below it.
# That is not an invented failure mode: it is exactly how `ctrl-←` shipped
# before `0052`, one line above where the modifier would have been used.
# **A FORCED FAILURE MUST ALSO BE FORCED TO FAIL.** The obvious way to write
# this helper is "the expected string is no longer on the screen, therefore the
# check bit" — and that is the SAME pass-on-absence mistake 1b was repaired for,
# one level up: a face that died on startup, a fixture that never reached the
# composer, or a mutant that did not build produces a blank screen and scores a
# triumphant forced failure. So the mutant screen has to be WITNESSED first: the
# text he typed has to be on it and the marker `Z` has to have landed somewhere.
# Only then does the marker being in the WRONG PLACE mean the arm was load-
# bearing. (Caught here because the first run of this helper printed an empty
# "composer reads:" line for `ctrl-→` and there was no way to tell, from a PASS,
# whether that screen had the text on it or nothing at all.)
# **THE WITNESS MUST NOT BE SOMETHING THE MUTATION CAN DESTROY**, and this is
# the second half of the lesson. The first version of this helper witnessed the
# TYPED TEXT — `alpha` — and `ctrl-→` promptly failed it: with that arm disarmed
# the marker lands at index 1 and the composer reads `aZlpha beta gamma`, in
# which the literal string `alpha` no longer occurs. The mutant had behaved
# exactly as predicted and the witness still called it unmeasured. A witness
# keyed on the thing under test is not a witness. So it is keyed on the
# COMPOSER BAND ITSELF — its own footer hint, which no key mutation can move —
# plus the marker having landed at all. Together those say "the face ran, the
# composer is up, and the keystrokes arrived", which is all a witness is for;
# WHERE the marker landed is the assertion's job, not the witness's.
# **AND IT MUST NOT BE SOMETHING THE COMPOSER'S OWN STATE CAN DESTROY EITHER.**
# This was `enter SENDS all` — the band's teaching row, removed the round the
# operator said he did not need instructions on how to use a textarea — and the
# obvious repair, the empty box's placeholder `type a message`, is WRONG for
# exactly the reason the paragraph above gives: every script here TYPES, so the
# placeholder is gone by the time the witness is read and the helper would
# report "the face never ran" on a face that ran perfectly. The witness is the
# HEADER's link word, which the face draws on every frame in every mode and
# which no mutation in `input.rs` can move. It says the face RAN and is DRAWING;
# the `Z` check below it says the keystrokes ARRIVED and a box took them, and
# the two together are what the paragraph above asks for.
COMPOSER_WITNESS="disconnected"
force_key() { # <label> <old> <new> <drive-script> <expected-string> <unused> <why>
    local label="$1" old="$2" new="$3" script="$4" expect="$5" why="$7" M MUT_OUT
    M="$(mutate "$INPUT_RS" "$old" "$new" "$label")"
    case "$M" in
        APPLIED*)
            build >/dev/null
            MUT_OUT="$(into "$script")"
            if ! screen "$MUT_OUT" | grep -qF "$COMPOSER_WITNESS"; then
                bad "$label: THE FACE IS NOT DRAWING ON THE MUTANT SCREEN — it did not run, did not build, or died on startup. Nothing was measured, and this is NOT a forced failure."
                screen "$MUT_OUT" | sed -n '1,12p' | sed 's/^/        /'
            elif ! screen "$MUT_OUT" | grep -qF "Z"; then
                bad "$label: the marker Z never landed on the mutant screen — the keystrokes did not arrive, so the caret's position was not measured"
                screen "$MUT_OUT" | sed -n '1,8p' | sed 's/^/        /'
            elif screen "$MUT_OUT" | grep -qF "$expect"; then
                bad "the $label check is NOT discriminating: '$expect' still appeared with the arm removed"
            else
                ok "$label forced to fail — $why"
                note "witnessed: the composer IS up and Z landed, just not at '$expect'"
                note "composer reads: $(screen "$MUT_OUT" | grep -m1 -F "Z" | sed 's/^ *//' | head -c 58)"
            fi ;;
        *) bad "$label mutation did not apply: $M" ;;
    esac
    restore; build >/dev/null
}

force_key "alt-←" \
    "KeyCode::Left if alt => Key::AltLeft," \
    "KeyCode::Left if alt && false => Key::AltLeft," \
    '(b"alpha beta gamma", 0.8), (b"\x1b[1;3D", 0.3), (b"\x1b[1;3D", 0.3), (b"Z", 0.8),' \
    "alpha Zbeta gamma" \
    "alpha" \
    "without the arm it flattens to a plain ← and moves by ONE CHARACTER, not one word"

force_key "alt-→" \
    "KeyCode::Right if alt => Key::AltRight," \
    "KeyCode::Right if alt && false => Key::AltRight," \
    '(b"alpha beta gamma", 0.8), (b"\x1b[1;3D", 0.3), (b"\x1b[1;3D", 0.3), (b"\x1b[1;3C", 0.3), (b"Z", 0.8),' \
    "alpha betaZ gamma" \
    "alpha" \
    "without the arm the return trip is one character, so the marker never reaches the word's end"

force_key "ctrl-→" \
    "KeyCode::Right if ctrl => Key::CtrlRight," \
    "KeyCode::Right if ctrl && false => Key::CtrlRight," \
    '(b"alpha beta gamma", 0.8), (b"\x1b[1;5D", 0.4), (b"\x1b[1;5C", 0.4), (b"Z", 0.8),' \
    "alpha beta gammaZ" \
    "alpha" \
    "without the arm it is a plain → and stops one character from the line start, not at the line end"

force_key "ctrl-↑" \
    "KeyCode::Up if ctrl => Key::CtrlUp," \
    "KeyCode::Up if ctrl && false => Key::CtrlUp," \
    '(b"one", 0.4), (b"\x1b\r", 0.3), (b"two", 0.6), (b"\x1b[1;5A", 0.4), (b"Z", 0.8),' \
    "Zone" \
    "one" \
    "without the arm it is a plain ↑: one line up, not the top of the draft"

force_key "ctrl-↓" \
    "KeyCode::Down if ctrl => Key::CtrlDown," \
    "KeyCode::Down if ctrl && false => Key::CtrlDown," \
    '(b"one", 0.4), (b"\x1b\r", 0.3), (b"two", 0.6), (b"\x1b[1;5A", 0.4), (b"\x1b[1;5B", 0.4), (b"Z", 0.8),' \
    "twoZ" \
    "two" \
    "without the arm it is a plain ↓: one line down, not the end of the draft"

# ─────────────────────────────────────────────────────────────────────────────
head_ "5g. FORCED — opt-delete, BOTH dialects, on the ONE arm they share"
# ─────────────────────────────────────────────────────────────────────────────
# **AND THE HONEST NOTE THAT GOES WITH IT.** The two byte spellings section 3
# drives (`ESC DEL` and `CSI 27;3;127~`) are decoded by the TERMINAL PARSER, and
# both arrive in this crate as Backspace+ALT — one arm covers the session
# whichever way it negotiated. So there is no per-dialect mutation to make here:
# the only counterpart this source can offer is the shared arm, and BOTH
# assertions must go red on it. Said plainly because "each dialect has its own
# forced failure" would be a claim this code cannot support. What the two
# spellings DO still prove is that both byte strings reach the parser at all,
# and that is a property of the terminal, not of a line we could mutate.
M="$(mutate "$INPUT_RS" \
    "KeyCode::Backspace if alt => Key::AltBackspace," \
    "KeyCode::Backspace if alt && false => Key::AltBackspace," \
    "opt-delete")"
case "$M" in
    APPLIED*)
        build >/dev/null
        for spelling in '\x1b\x7f:ESC-DEL' '\x1b[27;3;127~:CSI-27-3-127'; do
            BYTES="${spelling%%:*}"; NAME="${spelling##*:}"
            MUT_OUT="$(into "(b\"alpha beta gamma\", 0.8), (b\"$BYTES\", 0.8),")"
            if screen "$MUT_OUT" | grep -q "alpha beta" && ! screen "$MUT_OUT" | grep -q "alpha beta g"; then
                bad "the opt-delete ($NAME) check is NOT discriminating: a PLAIN backspace passed it"
            else
                ok "opt-delete ($NAME) forced to fail — flattened, it cuts ONE CHARACTER and leaves the rest of the word"
                note "composer reads: $(screen "$MUT_OUT" | grep -m1 alpha | sed 's/^ *//' | head -c 58)"
            fi
        done ;;
    *) bad "opt-delete mutation did not apply: $M" ;;
esac
restore; build >/dev/null

# ─────────────────────────────────────────────────────────────────────────────
head_ "5h. FORCED — the ? overlay's key list, which is GENERATED from the table"
# ─────────────────────────────────────────────────────────────────────────────
# "one table, two readers" is the claim section 3 ends on, and it had never been
# forced: the overlay listing every key is exactly what you also see if the
# check is simply looking at strings that happen to be there. So take a key OUT
# of the table and require the overlay to stop naming it. `alt-bksp` shares its
# row with `^W`, so removing it leaves a row that still renders and still reads
# sensibly — which is precisely how this could regress with nobody noticing.
M="$(mutate "$KEYS_RS" \
    "keys: KeySet::These(&[Key::Ctrl('w'), Key::AltBackspace])," \
    "keys: KeySet::These(&[Key::Ctrl('w')])," \
    "overlay-key-list")"
case "$M" in
    APPLIED*)
        build >/dev/null
        # **100x80, NOT 100x44, AND A POSITIVE CONTROL BELOW.**
        #
        # The witness ("cut the word before the cursor" — the act text of the
        # row the mutation deliberately leaves alone) sat one line from the
        # overlay's FOLD at 44. A thread's grammar is forty-odd bindings and
        # the overlay is scrollable precisely because it does not fit, so any
        # change that moves a row by one pushes the witness off and this check
        # reports "the overlay never opened" — a wrong diagnosis wearing the
        # shape of a real failure. `0072` tripped it by adding two root
        # bindings; the next round would have tripped it with something else.
        #
        # A taller frame fixes the symptom. The CONTROL below fixes the class:
        # it re-runs the same drive on the UNMUTATED binary and requires
        # `alt-bksp` to be PRESENT there. Without it, "alt-bksp is absent from
        # the mutant" is unfalsifiable — a row below the fold is absent too,
        # and so is a row that never rendered at any height.
        MUT_OUT="$(into '(b"\x1f", 1.2),' 100 80)"
        # WITNESS FIRST, same rule: an overlay that never opened also fails to
        # mention alt-bksp, and that is not this check biting. The witness is
        # the binding's `act` TEXT — what the overlay actually prints, and the
        # half of the row the mutation deliberately leaves alone. (It was first
        # written as the `short` label "kill word", which the overlay does not
        # render at all, so it failed a run in which the overlay was plainly up.
        # A witness has to be a string the screen really carries.)
        if ! has "$MUT_OUT" "cut the word before the cursor"; then
            bad "the overlay never opened on the mutant screen — nothing was measured, and this is NOT a forced failure"
            screen "$MUT_OUT" | sed -n '1,8p' | sed 's/^/        /'
        elif has "$MUT_OUT" "${MOD}bksp"; then
            bad "the overlay check is NOT discriminating: it still prints ${MOD}bksp with the key gone from the table"
        elif restore && build >/dev/null && ! has "$(into '(b"\x1f", 1.2),' 100 80)" "${MOD}bksp"; then
            # THE CONTROL. The real binary must PRINT `alt-bksp` on this exact
            # frame, or the mutant's silence proves nothing about the mutation.
            bad "CONTROL FAILED: the REAL overlay does not print ${MOD}bksp at 100x80 either — \
the absence on the mutant screen is not evidence of anything"
        else
            ok "the overlay key list forced to fail — a key taken out of the table stops being printed"
            note "and the row itself survives, which is how it would regress quietly: $(screen "$MUT_OUT" | grep -m1 'cut the word before the cursor' | sed 's/^ *//' | head -c 58)"
        fi ;;
    *) bad "overlay mutation did not apply: $M" ;;
esac
restore; build >/dev/null

# ─────────────────────────────────────────────────────────────────────────────
head_ "6. the restored crate, green"
# ─────────────────────────────────────────────────────────────────────────────
if diff -q "$BACKUP/app.rs" "$APP_RS" >/dev/null &&
   diff -q "$BACKUP/render.rs" "$RENDER_RS" >/dev/null &&
   diff -q "$BACKUP/hit.rs" "$HIT_RS" >/dev/null &&
   diff -q "$BACKUP/input.rs" "$INPUT_RS" >/dev/null; then
    ok "every mutated source restored byte-for-byte"
else
    bad "a source was left mutated — THE TREE IS DIRTY"
fi
if (cd "$REPO_ROOT" && cargo test -p tui -p tui-render 2>&1 | grep -q "FAILED"); then
    bad "cargo test is not green after the restore"
else
    ok "cargo test -p tui -p tui-render is green"
fi

printf '\n\033[1mresult\033[0m\n'
if [ "$FAILURES" -eq 0 ]; then
    printf '  \033[32mALL PROVEN\033[0m — the pointer and motion grammar answers the terminal correctly.\n'
else
    printf '  \033[31m%d check(s) failed\033[0m\n' "$FAILURES"
    exit 1
fi
