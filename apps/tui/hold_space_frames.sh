#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  HOLD TO SPEAK — proven on a REAL PTY, against the REAL binary.
#
#  **THE GESTURE:** in mic mode, a TAP of `space` toggles the recorder (the
#  behaviour he asked to KEEP — *"in addition to the toggle mode"*) and a HOLD
#  streams: the capture opens on the press, the auto-repeats are swallowed, and
#  the recording ENDS ON AN IDLE GAP because there is no key-release event in
#  his terminal. `0095` measured that: tmux 3.5a never forwards the kitty
#  keyboard push to iTerm2 (0 outward occurrences across four negotiations,
#  against a DCS-passthrough positive control that returned 1), and the only
#  channel that survives — xterm `modifyOtherKeys` — has no release concept.
#  Claude Code, the reference he named, does not use one either.
#
#  **WHY THIS SUITE EXISTS AND THE UNIT TESTS ARE NOT ENOUGH.** The unit tests
#  hand the detector a clock, which is what makes the repeat table exact. They
#  cannot see the app LOOP: that a held key really arrives as a stream of
#  separate space bytes through a pty, that the fifth clock is really armed
#  from the timer list, and that `Ev::HoldIdle` really reaches the face. Only a
#  real terminal driving the real binary shows that, and this is it.
#
#  **THE DISCRIMINATOR IS THE SAME KEYSTROKES AT TWO CADENCES.** Three presses
#  90 ms apart are one capture that ends by itself; three presses 700 ms apart
#  are three TAPS, which leave the recorder ON. Same bytes, opposite outcomes,
#  and no assertion here is satisfied by a face that simply died.
#
#  **IT NEVER OPENS THE MICROPHONE.** `TUI_MIC_CAPTURE` is pointed at a script
#  that writes a synthesised WAV — the boundary substitution `mic.rs`'s header
#  describes, not a fake microphone. `0046` forbids the real device: acquiring
#  it reconfigures the audio route and invalidates the AUI's open output
#  handle, and he is on that interface. It never contacts his daemon either —
#  `pty_drive.py` points the face's endpoint variables at a port nothing
#  listens on and refuses a live one.
#
#  It MUTATES source to force its own checks to fail, and it does so on an APFS
#  clone (`conformance_mirror.sh`), so the working tree is never written to and
#  a SIGKILL cannot leave a mutation behind.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
. "$CRATE_DIR/conformance_target.sh"
. "$CRATE_DIR/conformance_mirror.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/hold-space.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT INT TERM

PASS=0; FAIL=0
ok()    { PASS=$((PASS+1)); printf '  \033[32mPROVEN\033[0m  %s\n' "$1"; }
bad()   { FAIL=$((FAIL+1)); printf '  \033[31mFAILED\033[0m  %s\n' "$1"; }
head_() { printf '\n\033[1m── %s\033[0m\n' "$1"; }

BIN="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/tui"
DRIVE_PY="$CRATE_DIR/pty_drive.py"

# ── §0 · the anchors this suite reads the screen through ─────────────────────
#
# **AN ANCHOR THAT MOVED MUST SAY SO RATHER THAN QUIETLY PROVING NOTHING**
# (`0046`, and `pointer_frames.sh`'s shape rather than `conformance_test.sh`'s
# swallowed `assert`). Both strings live in `render.rs`, which this task does
# not own; if a sibling round renames them, this suite must abort loudly
# instead of reporting a face that is not recording.
head_ "0 · the screen anchors are where this suite thinks they are"
REC_ANCHOR='● RECORDING'
IDLE_ANCHOR='press r to record'
MISSING=0
for a in "$REC_ANCHOR" "$IDLE_ANCHOR"; do
  if ! grep -qF "$a" "$CRATE_DIR/../../libs/tui-render/src/render.rs"; then
    echo "  ANCHOR-MISSING mic-band-state [$a]"
    echo "  The mic band's state row no longer says this. THIS SUITE IS NOW"
    echo "  PROVING NOTHING — repair the anchor here rather than skipping it."
    MISSING=1
  fi
done
if [ $MISSING -eq 1 ]; then FAIL=$((FAIL+1)); else
  ok "the mic band still says '$REC_ANCHOR' and '$IDLE_ANCHOR'"
fi

(cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -3 >/dev/null)
[ -x "$BIN" ] || { echo "REFUSING TO RUN: no binary at $BIN"; exit 2; }

# ── the synthetic home and the injected legs ─────────────────────────────────
FAKEHOME="$WORK/home"
mkdir -p "$FAKEHOME/minds/proof/.mind"
cat > "$FAKEHOME/minds/proof/.mind/threads.json" <<'JSON'
{"threads":[{"id":"00000000-0000-4000-8000-00000000c0de",
             "session_id":"00000000-0000-4000-8000-00000000c0de",
             "summary":"the hold-to-speak proof thread",
             "last_turn_at":"2026-09-14T12:00:00Z","turn_count":1}]}
JSON

# Real audio, synthesised, so the dead-audio gate lets it through: the gate is
# not what this suite is about and a refusal would mask the gesture.
python3 - "$WORK/loud.wav" <<'PY'
import math, struct, sys, wave
w = wave.open(sys.argv[1], 'wb'); w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000)
w.writeframes(b''.join(struct.pack('<h', int(9000*math.sin(i*0.1))) for i in range(16000)))
w.close()
PY
printf 'the hold landed' > "$WORK/transcript.txt"
cat > "$WORK/capture.sh" <<EOF
#!/bin/sh
cp "$WORK/loud.wav" "\$1"
sleep 30
EOF
chmod +x "$WORK/capture.sh"
export TUI_MIC_CAPTURE="$WORK/capture.sh {}"
export TUI_MIC_STT="/bin/cat $WORK/transcript.txt"
export TUI_MIC_PLAY="/usr/bin/true"

# `↓` reaches the thread's row and `→` opens it — one press each, measured by
# `composer_frames.sh` against the face rather than assumed. `\x14` is `^T`,
# which is mic mode's door on his stack (`0115`: no `opt-<letter>` reaches this
# program at all, because macOS composes it in the keyboard layer).
DESCEND='(b"\x1b[B", 0.6), (b"\x1b[C", 1.4), (b"\x14", 0.6),'
drive()  { python3 "$DRIVE_PY" 100 24 "$BIN" "$FAKEHOME" "[$DESCEND $1]"; }
screen() { printf '%s' "$1" | sed -n 's/^SCREEN //p'; }
has()    { printf '%s' "$1" | grep -qF -- "$2"; }

# THE BURSTS. Three presses at each cadence: identical bytes, and the only
# difference is the wait between them.
HOLD='(b" ", 0.09), (b" ", 0.09), (b" ", 0.09), (b" ", 0.09), (b" ", 0.09), (b" ", 0.09), (b" ", 0.09), (b" ", 2.5)'
TAPS='(b" ", 0.7), (b" ", 0.7), (b" ", 2.5)'
ONE='(b" ", 2.5)'

# ── §1 · we are in mic mode at all ───────────────────────────────────────────
head_ "1 · the descent reaches mic mode, and the band is idle"
OUT="$(drive '(b"", 0.4)')"
if has "$OUT" "$IDLE_ANCHOR"; then
  ok "^T opened mic mode on a real pty and the band says it is idle"
else
  bad "never reached mic mode — the fixture or the descent is wrong"
  screen "$OUT" | sed -n '1,24p' | sed 's/^/        /'
fi

# ── §2 · A TAP OPENS THE MICROPHONE AND NOTHING CLOSES IT ────────────────────
#
# The WITNESS for every absence below: the face must be positively RECORDING
# after one press. An idle band here means the face died, not that the gesture
# worked.
head_ "2 · one press records, and no idle gap takes it away"
OUT_ONE="$(drive "$ONE")"
if has "$OUT_ONE" "$REC_ANCHOR"; then
  ok "a TAP opens the capture and it is STILL open 2.5 s later — the toggle he kept"
else
  bad "one press did not leave the recorder running (the tap toggle is broken)"
  screen "$OUT_ONE" | sed -n '1,24p' | sed 's/^/        /'
fi

# ── §3 · THREE PRESSES 700 ms APART ARE THREE TAPS ───────────────────────────
head_ "3 · the SLOW cadence — three presses 700 ms apart are three toggles"
OUT_TAPS="$(drive "$TAPS")"
if has "$OUT_TAPS" "$REC_ANCHOR"; then
  ok "start / stop / start: the third tap left the recorder ON — not read as a hold"
else
  bad "three deliberate taps did not end with the recorder on — presses were eaten"
  screen "$OUT_TAPS" | sed -n '1,24p' | sed 's/^/        /'
fi
# **AND THE MIDDLE TAP REALLY STOPPED SOMETHING.** Without this, a face where
# no press ever acted but the FIRST one would score the check above. The queue
# row is the positive fact only a completed stop can produce.
if has "$OUT_TAPS" "the hold landed"; then
  ok "…and the middle tap's capture completed and joined the queue"
else
  bad "the middle tap produced no queue row — only the first press acted"
  screen "$OUT_TAPS" | sed -n '1,24p' | sed 's/^/        /'
fi

# ── §4 · THE SAME PRESSES AT AUTO-REPEAT SPEED ARE ONE HOLD ──────────────────
head_ "4 · the FAST cadence — eight presses 90 ms apart are ONE capture that ends itself"
OUT_HOLD="$(drive "$HOLD")"
if has "$OUT_HOLD" "$REC_ANCHOR"; then
  bad "the microphone was still open 2.5 s after the repeats stopped — the idle gap never fired"
  screen "$OUT_HOLD" | sed -n '1,24p' | sed 's/^/        /'
else
  ok "the repeats ended the capture with no further keystroke — HE LET GO AND IT STOPPED"
fi
if has "$OUT_HOLD" "the hold landed"; then
  ok "…and the utterance reached the queue as ONE transcript, not eight fragments"
else
  bad "the hold produced no queue row — the capture never became text"
  screen "$OUT_HOLD" | sed -n '1,24p' | sed 's/^/        /'
fi

# ── §5 · FORCED: disarm the gesture and watch §4 become §3 ───────────────────
#
# **A GREEN YOU HAVE NOT FORCED TO FAIL IS NOT EVIDENCE.** With `HOLD_ARM`
# unreachable no burst is ever a hold, so the same eight presses become eight
# TOGGLES — an even number, ending with the recorder OFF, which §4's first
# check would score as a PASS. So the forced failure is keyed on the QUEUE ROW,
# which a machine-gunned toggle cannot produce: every fragment is shorter than
# a repeat interval and comes back empty.
head_ "5 · FORCED: with the gesture disarmed the same bytes do not produce an utterance"
ANCHOR='pub const HOLD_ARM: u32 = 3;'
MIC_SRC="$CRATE_DIR/src/mic.rs"
if ! grep -qF "$ANCHOR" "$MIC_SRC"; then
  echo "  ANCHOR-MISSING hold-arm"
  echo "  HOLD_ARM moved or changed value. THIS CHECK IS NOW PROVING NOTHING —"
  echo "  repair the anchor in hold_space_frames.sh rather than skipping it."
  FAIL=$((FAIL+1))
else
  python3 - "$MIC_SRC" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
old = "pub const HOLD_ARM: u32 = 3;"
assert old in s
open(p, 'w').write(s.replace(old, "pub const HOLD_ARM: u32 = 99; // CONFORMANCE PROBE", 1))
PY
  (cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -3 >/dev/null)
  OUT_MUT="$(drive "$HOLD")"
  if has "$OUT_MUT" "the hold landed"; then
    bad "the utterance arrived with the gesture disarmed — §4 is not testing the gesture"
  else
    ok "FORCED FAILURE OBSERVED: disarmed, the same eight presses produce no utterance"
  fi
  python3 - "$MIC_SRC" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
open(p, 'w').write(s.replace("pub const HOLD_ARM: u32 = 99; // CONFORMANCE PROBE",
                             "pub const HOLD_ARM: u32 = 3;", 1))
PY
  (cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -3 >/dev/null)
  OUT_BACK="$(drive "$HOLD")"
  if has "$OUT_BACK" "the hold landed"; then
    ok "the source is restored and the gesture works again"
  else
    bad "THE RESTORE DID NOT TAKE — check $MIC_SRC"
  fi
fi

# ── §6 · nothing of this run survives in the tree ────────────────────────────
head_ "6 · the working tree was never written to"
if grep -rn 'CONFORMANCE PROBE' "$CONFORMANCE_MIRROR_REAL_ROOT/apps/tui/src" \
     "$CONFORMANCE_MIRROR_REAL_ROOT/libs/tui-render/src" >/dev/null 2>&1; then
  bad "a mutation marker is in the REAL tree — the mirror did not hold"
else
  ok "no mutation marker in the real tree; every write went to the APFS clone"
fi

printf '\n\033[1m%s\033[0m\n' "── hold-space conformance: $PASS proven, $FAIL failed"
[ $FAIL -eq 0 ] || exit 1
