#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  MIC MODE'S CONFORMANCE SUITE
#
#  **A green you have not forced to fail is not evidence** (`core:writing-tools`).
#  Everything here is either a REAL measurement against the real binaries, or a
#  mutation of the shipped source that a check must go RED for.
#
#  THE ONE THING THIS SUITE EXISTS FOR
#  ───────────────────────────────────
#  `whisper` returns the word `you`, at EXIT 0, on digital silence. An
#  ungranted, blocked or dead microphone therefore does not error — it
#  FABRICATES a plausible English word that the face would show him as his own
#  speech. §2 puts the bare transcriber and mic mode side by side on the SAME
#  silent file and requires them to disagree. §3 removes the gate and requires
#  §2 to go red with the fabricated word in hand.
#
#  WHAT IT NEVER DOES
#  ──────────────────
#  * **It never opens the microphone.** Every WAV is synthesised — `python3`
#    writes the silence, `say` writes the speech. The live-device leg is
#    UNPROVEN and this suite does not pretend otherwise.
#  * **It never plays anything audibly.** `afplay` runs at `-v 0` throughout.
#  * **It never contacts the threads daemon.** Nothing here has a socket.
#
#  IT MUTATES REAL SOURCES AND RESTORES THEM ON AN EXIT TRAP.
#  Do not SIGKILL it. If a previous run was killed mid-mutation, §0 refuses to
#  start and tells you which file to restore.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

cd "$(dirname "$0")" || exit 1
ROOT="$(pwd)"

# ── BUILD INTO A CONFORMANCE-ONLY TARGET DIRECTORY ───────────────────────────
# The operator's LIVE face executes `<repo>/target/debug/tui`, and the cargo
# invocations below rebuild whatever `CARGO_TARGET_DIR` names. Sourcing this
# exports a target directory that is NOT the live one and REFUSES to run if the
# resolved one ever is. The whole argument is in that file. It also settles §0's
# `TARGET_DIR` question below: `cargo metadata` is still the authority, and it
# now answers with this directory because the variable is exported before it is
# asked.
REPO_ROOT="$(cd "$ROOT/../.." && pwd)"
. "$ROOT/conformance_target.sh"

# ── AND MUTATE A COPY, NEVER THE WORKING TREE ────────────────────────────────
# This suite MUTATES source to prove its checks can still detect the defects
# they exist for. A trap restores the tree on every exit path it can catch —
# and `SIGKILL` is not one of them. On 2026-09-22 a worker was killed between a
# `mutate` and its `restore` and left a live mutation in `src/transport.rs`,
# which then stood as the tree the next worker would measure. Sourcing this
# re-points `CRATE_DIR`/`REPO_ROOT` at a cheap APFS clone of the workspace, so
# every mutation and every build below lands on the copy. The working tree is
# never written to, which is why the property survives being killed.
. "$ROOT/conformance_mirror.sh"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/mic-conformance.XXXXXX")"

MIC_SRC="src/mic.rs"
APP_SRC="src/app.rs"
BAK="$SCRATCH/backup"
mkdir -p "$BAK"

PASS=0
FAIL=0

cleanup() {
  # **RESTORE FIRST, ALWAYS.** A killed run that left a mutated source behind is
  # a tree the next worker inherits broken.
  for f in "$MIC_SRC" "$APP_SRC"; do
    b="$BAK/$(echo "$f" | tr '/' '_')"
    [ -f "$b" ] && cp "$b" "$f"
  done
  rm -rf "$SCRATCH"
}
# The scratch target tree goes with it: bash's traps are last-write-wins, so a
# suite that installs its own EXIT trap would otherwise silently discard the
# one `conformance_target.sh` set and leave half a gigabyte behind.
trap 'cleanup; conformance_target_release' EXIT INT TERM

say_head() { printf '\n\033[1m── %s\033[0m\n' "$1"; }
ok()   { PASS=$((PASS+1)); printf '  \033[32mPROVEN\033[0m  %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAILED\033[0m  %s\n' "$1"; }

# ── §0 · refuse to run over somebody else's mutation ─────────────────────────
say_head "0 · the tree is clean and the crate is green"

if grep -rn 'CONFORMANCE PROBE' src render/src 2>/dev/null; then
  echo "REFUSING TO RUN: a source above still carries a mutation marker."
  echo "A previous run was killed before its trap restored it. Restore it first."
  exit 3
fi
for f in "$MIC_SRC" "$APP_SRC"; do
  cp "$f" "$BAK/$(echo "$f" | tr '/' '_')"
done

cargo build -p tui --example mic_probe --offline >/dev/null 2>&1 \
  || { echo "the probe does not build; nothing below can mean anything"; exit 2; }

# **THE TARGET DIRECTORY IS ASKED FOR, NOT GUESSED** (`0065`). This line used to
# be a hard-coded `$ROOT/../../target`, so with `CARGO_TARGET_DIR` set — a
# setting every other suite in this crate respects — the build above SUCCEEDED,
# the binary landed somewhere else, and the suite exited 2 saying "no probe".
# That is not a refusal with a reason; it is a suite that cannot run at all
# under a normal configuration. Cargo is the authority on where it puts things,
# so cargo is asked: this also picks up a `build.target-dir` in a cargo config,
# which neither the env var nor a relative path would. The env var and the repo
# default remain fallbacks for a cargo too old or too broken to answer.
TARGET_DIR="$(cargo metadata --offline --no-deps --format-version 1 2>/dev/null |
  python3 -c 'import json,sys
try:
    print(json.load(sys.stdin)["target_directory"])
except Exception:
    pass' 2>/dev/null)"
[ -n "$TARGET_DIR" ] || TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/../../target}"
PROBE="$TARGET_DIR/debug/examples/mic_probe"

# **THE LOUD REFUSAL STAYS**, because it is the right answer to the genuinely
# unresolvable case — it just has to say enough to be actionable. A silent or
# mysterious exit 2 here is the same class of defect as a check that passes on
# an absent string: nothing below it would have measured anything.
[ -x "$PROBE" ] || {
  echo "REFUSING TO RUN: no mic_probe at $PROBE"
  echo "  target directory resolved to : $TARGET_DIR"
  echo "  CARGO_TARGET_DIR             : ${CARGO_TARGET_DIR:-<unset>}"
  echo "  cwd                          : $ROOT"
  echo "  The build above reported SUCCESS, so this is a path this suite could"
  echo "  not resolve rather than a build failure. Every check below would be"
  echo "  measuring nothing, so it stops here."
  exit 2; }
ok "the probe builds and no source carries a stale mutation"

# ── the fixtures. SYNTHESISED, never recorded ────────────────────────────────
SILENCE="$SCRATCH/silence.wav"
SPEECH="$SCRATCH/speech.wav"
python3 - "$SILENCE" <<'PY'
import sys, wave
w = wave.open(sys.argv[1], 'wb')
w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000)
w.writeframes(b'\x00\x00' * 16000)   # one second of digital silence
w.close()
PY
say -o "$SCRATCH/speech.aiff" "Sweep the resize set points after the fix lands." 2>/dev/null
afconvert -f WAVE -d LEI16@16000 -c 1 "$SCRATCH/speech.aiff" "$SPEECH" 2>/dev/null
[ -s "$SILENCE" ] && [ -s "$SPEECH" ] || { echo "could not synthesise the fixtures"; exit 2; }

RETRANSCRIBE="$HOME/plugins/core/tools/retranscribe"
HAVE_ENGINE=0
if [ -x "$RETRANSCRIBE" ] && "$RETRANSCRIBE" --check >/dev/null 2>&1; then
  HAVE_ENGINE=1
fi

# ── §1 · the REAL transcription leg ──────────────────────────────────────────
say_head "1 · real speech, real whisper, through mic mode's own code path"

if [ "$HAVE_ENGINE" = 0 ]; then
  bad "NO TRANSCRIPTION ENGINE ON THIS MACHINE — §1 to §3 cannot be evaluated."
  echo "     This is a CANNOT-CHECK and never a pass. Install/repair the engine"
  echo "     that \`retranscribe --check\` resolves, then re-run."
else
  OUT="$("$PROBE" stt "$SPEECH" 2>&1)"; RC=$?
  if [ $RC -eq 0 ] && [ "${OUT#TRANSCRIPT }" != "$OUT" ] && echo "$OUT" | grep -qi 'resize'; then
    ok "a real utterance transcribes through the gate: ${OUT:0:60}"
  else
    bad "real speech did not transcribe (exit $RC): $OUT"
  fi

  # ── §2 · THE TRAP, side by side on ONE file ──────────────────────────────
  say_head "2 · THE SILENCE TRAP — the bare transcriber and mic mode must disagree"

  BARE="$("$RETRANSCRIBE" "$SILENCE" 2>/dev/null)"; BARE_RC=$?
  echo "     bare retranscribe on silence : exit=$BARE_RC stdout=[$BARE]"
  if [ $BARE_RC -eq 0 ] && [ -n "$BARE" ]; then
    ok "THE HAZARD IS LIVE: silence returns '$BARE' at exit 0, so exit 0 is not validity"
  else
    # If this ever stops being true the gate is no longer load-bearing, and
    # that is a finding rather than a pass.
    bad "the hazard did not reproduce (exit $BARE_RC, stdout [$BARE]) — re-read the gate's premise"
  fi

  MINE="$("$PROBE" stt "$SILENCE" 2>&1)"; MINE_RC=$?
  echo "     mic mode on the same file    : exit=$MINE_RC [$MINE]"
  if [ $MINE_RC -eq 2 ] && echo "$MINE" | grep -q 'heard nothing'; then
    ok "mic mode REFUSES the same silence and says it heard nothing"
  else
    bad "mic mode did not refuse digital silence (exit $MINE_RC): $MINE"
  fi
  if echo "$MINE" | grep -qx "TRANSCRIPT $BARE"; then
    bad "MIC MODE RETURNED THE FABRICATED WORD — the gate is not in the path"
  else
    ok "the fabricated word never reached mic mode's caller"
  fi

  # ── §3 · FORCE IT TO FAIL ────────────────────────────────────────────────
  say_head "3 · FORCED: remove the gate and watch §2 turn into the defect"

  ANCHOR='    let peak = gate(&bytes)?;'
  if ! grep -qF "$ANCHOR" "$MIC_SRC"; then
    echo "  ANCHOR-MISSING gate-call"
    echo "  The gate call moved. THIS CHECK IS NOW PROVING NOTHING — repair the"
    echo "  anchor in mic_conformance_test.sh rather than skipping this section."
    FAIL=$((FAIL+1))
  else
    python3 - "$MIC_SRC" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = "    let peak = gate(&bytes)?;"
assert old in s
s = s.replace(old, "    let peak = gate(&bytes).unwrap_or(0); // CONFORMANCE PROBE", 1)
open(p, 'w').write(s)
PY
    cargo build -p tui --example mic_probe --offline >/dev/null 2>&1
    MUT="$("$PROBE" stt "$SILENCE" 2>&1)"; MUT_RC=$?
    echo "     with the gate removed        : exit=$MUT_RC [$MUT]"
    if [ $MUT_RC -eq 0 ] && echo "$MUT" | grep -q "TRANSCRIPT"; then
      ok "FORCED FAILURE OBSERVED: without the gate, silence becomes '${MUT#TRANSCRIPT }'"
    else
      bad "removing the gate changed nothing — §2 is not testing the gate"
    fi
    cp "$BAK/$(echo "$MIC_SRC" | tr '/' '_')" "$MIC_SRC"
    cargo build -p tui --example mic_probe --offline >/dev/null 2>&1
    RE="$("$PROBE" stt "$SILENCE" 2>&1)"; RE_RC=$?
    if [ $RE_RC -eq 2 ]; then
      ok "the source is restored and the refusal is back"
    else
      bad "THE RESTORE DID NOT TAKE — the tree may be dirty; check $MIC_SRC"
    fi
  fi
fi

# ── §4 · the app-level suite, and ITS forced failure ─────────────────────────
say_head "4 · the app-level pipeline, and the same mutation seen from the face"

if cargo test -p tui --offline --test mic >/dev/null 2>&1; then
  ok "tests/mic.rs is green (7 checks: gate, send-block, full pipeline, retry, play, mode gate, empty capture)"
else
  bad "tests/mic.rs is RED before any mutation — nothing below can mean anything"
fi

ANCHOR='    let peak = gate(&bytes)?;'
if ! grep -qF "$ANCHOR" "$MIC_SRC"; then
  echo "  ANCHOR-MISSING gate-call-app"
  FAIL=$((FAIL+1))
else
  python3 - "$MIC_SRC" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
s = s.replace("    let peak = gate(&bytes)?;",
              "    let peak = gate(&bytes).unwrap_or(0); // CONFORMANCE PROBE", 1)
open(p, 'w').write(s)
PY
  OUT="$(cargo test -p tui --offline --test mic a_silent_capture_is_refused 2>&1)"
  if echo "$OUT" | grep -q 'THE SILENCE TRAP FIRED'; then
    ok "FORCED: with the gate gone the QUEUE ROW reads 'you' and the test names it"
  else
    bad "the app-level silence test did not catch the removed gate"
  fi
  cp "$BAK/$(echo "$MIC_SRC" | tr '/' '_')" "$MIC_SRC"
fi

# ── §5 · playback, real, silent ──────────────────────────────────────────────
say_head "5 · playback — the real afplay, at volume 0"

OUT="$("$PROBE" play "$SPEECH" 2>&1)"; RC=$?
if [ $RC -eq 0 ] && [ "$OUT" = "PLAYED" ]; then
  ok "a real capture plays through the detached spawn path"
else
  bad "playback of a real file failed (exit $RC): $OUT"
fi

OUT="$("$PROBE" play "$SCRATCH/not-here.wav" 2>&1)"; RC=$?
if [ $RC -eq 2 ] && echo "$OUT" | grep -q 'REFUSED'; then
  ok "FORCED: a missing capture is refused by name, not silently (exit 1 from afplay)"
else
  bad "a missing capture did not produce a named refusal (exit $RC): $OUT"
fi

# ── §6 · the header repair recovers words that would otherwise be lost ───────
say_head "6 · a capture severed mid-recording is REPAIRED, not lost"

SEVERED="$SCRATCH/severed.wav"
cp "$SPEECH" "$SEVERED"
python3 - "$SEVERED" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, 'rb').read())
i = b.find(b'data')
b[i+4:i+8] = (0).to_bytes(4, 'little')   # the killed-writer signature
b[4:8] = (0).to_bytes(4, 'little')
open(p, 'wb').write(bytes(b))
PY

if [ "$HAVE_ENGINE" = 1 ]; then
  "$RETRANSCRIBE" "$SEVERED" >/dev/null 2>&1; RC=$?
  if [ $RC -ne 0 ]; then
    ok "the hazard is live: a severed capture is exit $RC to the bare transcriber — his words would be lost"
  else
    bad "a severed capture transcribed anyway; the repair may no longer be load-bearing"
  fi
fi
afplay -v 0 "$SEVERED" >/dev/null 2>&1; RC=$?
if [ $RC -ne 0 ]; then
  ok "…and afplay refuses it too (exit $RC)"
else
  bad "afplay accepted a zero-declaring WAV"
fi

"$PROBE" repair "$SEVERED" >/dev/null 2>&1
if [ "$HAVE_ENGINE" = 1 ]; then
  OUT="$("$PROBE" stt "$SEVERED" 2>&1)"; RC=$?
  if [ $RC -eq 0 ] && echo "$OUT" | grep -qi 'resize'; then
    ok "after the repair the SAME file transcribes correctly — the words are recovered"
  else
    bad "the repair did not recover the capture (exit $RC): $OUT"
  fi
fi

# A capture whose header is honest must be left alone.
OUT="$("$PROBE" repair "$SPEECH" 2>&1)"
if [ "$OUT" = "INTACT" ]; then
  ok "an honest header is not rewritten"
else
  bad "the repair touched a file it should not have: $OUT"
fi

# ── §7 · the tree is as we found it ──────────────────────────────────────────
say_head "7 · every mutated source restored byte-for-byte"

DIRTY=0
for f in "$MIC_SRC" "$APP_SRC"; do
  b="$BAK/$(echo "$f" | tr '/' '_')"
  if ! cmp -s "$b" "$f"; then
    bad "$f WAS LEFT MUTATED — THE TREE IS DIRTY"
    DIRTY=1
  fi
done
if grep -rn 'CONFORMANCE PROBE' src render/src >/dev/null 2>&1; then
  bad "a mutation marker survived in the tree"
  DIRTY=1
fi
[ $DIRTY -eq 0 ] && ok "every source restored byte-for-byte, no markers left"

printf '\n\033[1m%s\033[0m\n' "── mic conformance: $PASS proven, $FAIL failed"
[ $FAIL -eq 0 ] || exit 1
