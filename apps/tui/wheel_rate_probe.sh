#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  THE WHEEL RATE TABLE — burst rate in, ROWS MOVED out, measured on a REAL PTY
#  against the REAL BINARY.
#
#  **WHY THIS EXISTS AND WHY A UNIT TEST IS NOT A SUBSTITUTE.**
#
#  `0097`'s brief asked for throughput proportional to how hard he scrolls, and
#  said in as many words that *a unit test will lie to you here*. It is right,
#  and the reason is specific rather than general: the curve reads a CLOCK, and
#  every unit test of it hands `on_input_at` a synthetic instant. Those tests
#  prove the arithmetic. They cannot prove that macOS's wheel reports, arriving
#  through a terminal, batched by whatever the kernel hands one `read()`, charge
#  the meter the way the arithmetic assumes — because the batching IS the input
#  and a synthetic clock replaces it with an assumption.
#
#  So this probe sends real SGR-1006 wheel reports into a real pty at graded
#  rates and reads back what the face actually did with them.
#
#  # How ROWS MOVED is measured, and why it is not read off the screen
#
#  The honest screen measurement — count rows of text that moved — has a
#  ceiling: every surface the wheel reaches is shorter than a hard flick's
#  travel, so at the top of the curve the face is against a saturating end and
#  the screen says "as far as it goes" for every rate above some threshold.
#  That is the measurement going blind exactly where the question lives.
#
#  So the face SAYS what it moved. `App::flush_wheel` emits one `wheel` debug
#  line per run — `notches`, `rows`, `gain` — which is the number the curve
#  produced for reports that really did arrive over a terminal at a real rate.
#  The face is a normal build, driven normally, writing its normal log to
#  `$HOME/.tui/log`; nothing here is a test hook and the line is useful to the
#  operator re-tuning the curve on his own face.
#
#  Section 3 is what keeps that honest: the same numbers, with the curve
#  MUTATED FLAT, must collapse to one row a notch. A table that cannot be made
#  to lie is a table nobody has checked.
#
#  # The rates, and the one the driver cannot reach
#
#  Rate is notches-per-chunk over the gap after it. `pty_drive.py` waits with a
#  50 ms `select` granularity, so 50 ms is the shortest honest gap and the rates
#  below are built by putting MORE notches in a chunk rather than by shortening
#  the gap. The top row is the real thing — a whole flick inside ONE `read()`,
#  which is what the trackpad actually delivers and which has no meaningful
#  denominator at all.
#
#  Usage:  ./wheel_rate_probe.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
# ── BUILD INTO A CONFORMANCE-ONLY TARGET DIRECTORY ───────────────────────────
# The operator's LIVE face executes `$REPO_ROOT/target/debug/tui`. Sourcing this
# exports a target directory that is NOT the live one and REFUSES to run if the
# resolved one ever is.
. "$CRATE_DIR/conformance_target.sh"

# ── AND MUTATE A COPY, NEVER THE WORKING TREE ────────────────────────────────
# Section 3 mutates `src/app.rs` to force this table to lie. A trap cannot catch
# `SIGKILL`, and a worker killed between a mutation and its restore is exactly
# how `src/transport.rs` was left poisoned on 2026-09-22. Sourcing this re-points
# `CRATE_DIR`/`REPO_ROOT` at a cheap APFS clone of the workspace, so the working
# tree is never written to at all.
. "$CRATE_DIR/conformance_mirror.sh"

BIN="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/tui"
DRIVE_PY="$CRATE_DIR/pty_drive.py"
APP_RS="$CRATE_DIR/src/app.rs"
WORK="$(mktemp -d)"
BACKUP="$WORK/backup"; mkdir -p "$BACKUP"
cp "$APP_RS" "$BACKUP/app.rs"
FAILURES=0

cleanup() {
    [ -f "$BACKUP/app.rs" ] && cp "$BACKUP/app.rs" "$APP_RS"
    rm -rf "$WORK"
}
trap 'cleanup; conformance_target_release' EXIT INT TERM

ok()    { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
bad()   { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
note()  { printf '        %s\n' "$1"; }
head_() { printf '\n\033[1m%s\033[0m\n' "$1"; }
restore() { cp "$BACKUP/app.rs" "$APP_RS"; }
build() { (cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -3); }

# ── the synthetic home ───────────────────────────────────────────────────────
# Six projects, so the root is a list a wheel can move a cursor through, and so
# the face has something to draw. The transcript is not needed: the meter is
# charged in `flush_wheel`, ABOVE the fork where a transcript's viewport parts
# company with a menu's cursor, so the rows a run is worth are the same number
# on every surface. That is the "one mechanism, two mount points" property, and
# measuring it on the root is measuring it for the transcript too.
FAKEHOME="$WORK/home"
for i in 1 2 3 4 5 6; do mkdir -p "$FAKEHOME/minds/proof$i/.mind"; done

wheel()    { printf '(b"\\x1b[<65;10;10M", 0.0),'; }
wheelrun() { local n=$1 out=""; for _ in $(seq "$n"); do out="$out$(wheel)"; done; printf '%s' "$out"; }
# A burst of <n> notches inside ONE chunk, then <gap> seconds of quiet.
burst()    { printf '%s(b"", %s),' "$(wheelrun "$1")" "$2"; }

# ── the script: one face, every rate, separated by REST ──────────────────────
# `WHEEL_TAU` is 150 ms and the charge decays as `exp(-dt/tau)`, so 1.5 s is ten
# time constants — the meter is at 0.005% of its peak and every burst below
# starts from rest. Without that the table would measure the tail of the
# previous row rather than the rate of the current one.
REST=1.5
SCRIPT="["
for spec in "1 0.500" "1 0.200" "1 0.100" "1 0.050" "2 0.050" "4 0.050" "8 0.050"; do
    set -- $spec
    # Six repeats so the meter reaches its steady state at this rate rather than
    # being measured on its first, still-charging run.
    for _ in 1 2 3 4 5 6; do SCRIPT="$SCRIPT$(burst "$1" "$2")"; done
    SCRIPT="$SCRIPT(b\"\", $REST),"
done
# And the real thing: forty notches in ONE read, which is what a hard flick is.
SCRIPT="$SCRIPT$(burst 40 1.5)]"

# ── run it ───────────────────────────────────────────────────────────────────
run_probe() { # -> prints "<iso-timestamp> <notches> <rows>" per flushed run
    rm -rf "$FAKEHOME/.tui"
    TUI_LOG="tui::app=debug" python3 "$DRIVE_PY" 100 24 "$BIN" "$FAKEHOME" "$SCRIPT" >/dev/null 2>&1
    cp "$FAKEHOME/.tui/log" "$WORK/${1:-run}.log" 2>/dev/null
    sed -n 's/^\([0-9T:.-]*Z\).*wheel_run notches=\([0-9]*\) rows=\([0-9]*\).*/\1 \2 \3/p' \
        "$FAKEHOME/.tui/log" 2>/dev/null
}

head_ "0. the face builds and the probe can see its wheel runs"
build >/dev/null
[ -x "$BIN" ] || { bad "no binary at $BIN"; exit 2; }
REAL="$(run_probe real)"
COUNT="$(printf '%s\n' "$REAL" | grep -c . )"
if [ "$COUNT" -lt 10 ]; then
    bad "only $COUNT wheel runs reached the face — the probe measured nothing, and every number below would be an artefact of that"
    printf '%s\n' "$REAL" | sed 's/^/        /'
    exit 1
fi
ok "$COUNT wheel runs observed on the pty"

# ── the table ────────────────────────────────────────────────────────────────
# Rows are grouped back onto their rate by the notch counts, in order, because
# the script's shape is known and the log's order is the wire's order.
# **THE TABLE IS SEGMENTED BY THE OBSERVED CLOCK, NOT BY THE SCRIPT'S PLAN.**
#
# The first cut of this grouped runs back onto the rate the script ASKED for, by
# notch count. It was wrong, and measuring rather than assuming is what showed
# it: under the real curve a run moves tens of rows, the projection after it is
# more expensive, and the next chunks then sit in the pty buffer and arrive
# COALESCED — so the face saw notch counts the script never sent. The plan is
# what went in; only the log says what arrived.
#
# So bursts are separated by the 1.5 s rests (ten `WHEEL_TAU`, the meter back at
# rest) and each burst's rate comes from ITS OWN timestamps. That is the rate the
# face actually experienced, which is the only rate the curve ever saw.
#
# The helper is written to a FILE rather than passed with `python3 -c`, because
# a program with quotes in it inside a shell string is a quoting accident
# waiting to happen — and it already was one.
cat > "$WORK/bursts.py" <<'BURSTS'
import sys, datetime as dt

MODE = sys.argv[1] if len(sys.argv) > 1 else "table"

runs = []
for line in sys.stdin:
    p = line.split()
    if len(p) != 3:
        continue
    runs.append((dt.datetime.fromisoformat(p[0].replace("Z", "+00:00")),
                 int(p[1]), int(p[2])))
if not runs:
    print("?" if MODE != "table" else "no wheel runs in the log")
    raise SystemExit(0)

bursts, cur = [], [runs[0]]
for prev, this in zip(runs, runs[1:]):
    if (this[0] - prev[0]).total_seconds() > 1.0:
        bursts.append(cur)
        cur = []
    cur.append(this)
bursts.append(cur)

if MODE in ("first", "last"):
    b = bursts[0] if MODE == "first" else bursts[-1]
    print(sum(r[1] for r in b), sum(r[2] for r in b))
    raise SystemExit(0)

print("   observed rate |  notches |   rows | rows/notch | reads")
for b in bursts:
    n = sum(r[1] for r in b)
    rows = sum(r[2] for r in b)
    span = (b[-1][0] - b[0][0]).total_seconds()
    rate = ("%10.0f/s" % (n / span)) if span > 0.02 else "  ONE READ"
    print("%16s | %8d | %6d | %10.2f | %d" % (rate, n, rows, rows / n, len(b)))
BURSTS

table() { printf '%s\n' "$1" | python3 "$WORK/bursts.py" table; }
edge()  { printf '%s\n' "$1" | python3 "$WORK/bursts.py" "$2"; }

head_ "1. THE RATE TABLE — measured on a pty, against the real binary"
table "$REAL" | sed 's/^/        /'

head_ "2. the two properties he asked for, read off that table"
read -r SLOW_N SLOW_ROWS <<<"$(edge "$REAL" first)"
read -r FAST_N FAST_ROWS <<<"$(edge "$REAL" last)"
if [ "$SLOW_N" = "$SLOW_ROWS" ]; then
    ok "ONE DELIBERATE NOTCH IS EXACTLY ONE ROW on a real pty ($SLOW_N notches at the slowest rate moved $SLOW_ROWS rows)"
else
    bad "the slowest burst moved $SLOW_ROWS rows for $SLOW_N notches — the floor is not holding on the glass"
fi
note "the hardest flick: $FAST_N notches in one read moved $FAST_ROWS rows"
if [ "${FAST_ROWS:-0}" -gt 200 ]; then
    ok "A HARD FLICK CROSSES A LONG TRANSCRIPT — $FAST_ROWS rows out of one gesture"
else
    bad "the hardest flick moved only ${FAST_ROWS:-?} rows: that is still 'sooo slow'"
fi

# ── FORCED ───────────────────────────────────────────────────────────────────
head_ "3. FORCED — flatten the curve and watch the table collapse"
# **A GREEN YOU HAVE NOT FORCED TO FAIL IS NOT EVIDENCE.** The mutation is
# exactly the thing he ruled out in writing — "I don't want you to solve it with
# a bigger step size" — inverted: make the gain a constant 1, which is the face
# as it behaved before the curve existed. Every row of the table must collapse
# to one row per notch, and the flick must stop crossing anything.
python3 - "$APP_RS" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
old = "        1.0 + (WHEEL_GAIN_MAX - 1.0) * t * t"
if s.count(old) != 1:
    sys.stderr.write("ANCHOR-MISSING gain(): matched %d times\n" % s.count(old)); sys.exit(3)
open(p, "w").write(s.replace(old, "        { let _ = t; 1.0 }", 1))
PY
case $? in
  0) build >/dev/null
     FLAT="$(run_probe flat)"
     table "$FLAT" | sed 's/^/        /'
     read -r FF_N FF_ROWS <<<"$(edge "$FLAT" last)"
     if [ "$FF_N" = "$FF_ROWS" ]; then
         ok "with the curve flattened the hardest flick moves $FF_ROWS rows for $FF_N notches — one row a notch, where the real curve moved $FAST_ROWS. The table IS measuring the curve."
     else
         bad "the flattened build still moved $FF_ROWS rows for $FF_N notches: this probe is NOT discriminating"
     fi ;;
  *) bad "ANCHOR-MISSING in app.rs gain(): the curve moved and this forced failure could not be applied. Section 2 above is SUSPENDED, not passed - re-key the anchor." ;;
esac
restore; build >/dev/null

head_ "4. the working tree"
if cmp -s "$BACKUP/app.rs" "$APP_RS"; then
    ok "app.rs restored byte-for-byte (and it was a MIRROR's app.rs throughout — see conformance_mirror.sh)"
else
    bad "app.rs is not back as it was"
fi

if [ "$FAILURES" -eq 0 ]; then
    printf '\n\033[32mALL GREEN\033[0m — the rate table was measured on a pty and forced to collapse.\n'
    exit 0
fi
printf '\n\033[31m%d FAILURE(S)\033[0m\n' "$FAILURES"
exit 1
