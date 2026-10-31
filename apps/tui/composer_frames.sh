#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  tui COMPOSER conformance test — THE PTY LAYER, and it is the one the
#  research's survey said nobody had.
#
#  Everything below is a fact only a terminal can answer: which bytes a key
#  ACTUALLY arrives as on this stack, what crossterm makes of them, and what the
#  face does with what crossterm made. The unit tests prove the model; the
#  property tests prove the laws; this proves the WIRE — and both defects this
#  round was built around were found at exactly this layer and could not have
#  been found at any other.
#
#  `core:writing-tools`: a green you have not forced to fail is not evidence.
#  Sections 2 and 6 are FORCED FAILURES — the first asserts a defect that is
#  live in a dependency right now, so it goes red the day that dependency is
#  fixed (which is when the capability should be switched on); the second
#  mutates our own source to remove the paste rule and requires the suite to
#  catch it.
#
#  WHAT IT TOUCHES: nothing outside this crate, and NO DAEMON. The face's
#  endpoint variables (`TUI_THREADS_CHASSIS_PORT`, `TUI_THREADS_PORT`,
#  `AUI_V2_URL`, `CAULDRON_BIN`; pinned by `tests/drive_isolation.rs`) point
#  at port 1, a port nothing listens on, so the send
#  is REFUSED at the transport before a byte leaves — which is also how section 3
#  proves that `enter` reached the send path without one ever being fired at his
#  real threads. `$HOME` is redirected to a scratch directory for the whole run,
#  so the project list, the draft journal and the log are all synthetic and his
#  own are neither read nor written.
#
#  Usage:  ./composer_frames.sh
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
WORK="$(mktemp -d)"
INPUT_RS="$CRATE_DIR/src/input.rs"
VT_RS="$CRATE_DIR/src/vt.rs"
BACKUP="$WORK/backup"
FAILURES=0

mkdir -p "$BACKUP"
cp "$INPUT_RS" "$BACKUP/input.rs"
cp "$VT_RS" "$BACKUP/vt.rs"
cleanup() {
    [ -f "$BACKUP/input.rs" ] && cp "$BACKUP/input.rs" "$INPUT_RS"
    [ -f "$BACKUP/vt.rs" ] && cp "$BACKUP/vt.rs" "$VT_RS"
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

build() { (cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -3); }

# ── the synthetic home ───────────────────────────────────────────────────────
# One project, one thread, so the descent into a composer is deterministic:
# `→` opens the project, `→` opens the thread. His own `~/minds` is never read.
FAKEHOME="$WORK/home"
mkdir -p "$FAKEHOME/minds/proof/.mind"
cat > "$FAKEHOME/minds/proof/.mind/threads.json" <<'JSON'
{"threads":[{"id":"00000000-0000-4000-8000-00000000c0de",
             "session_id":"00000000-0000-4000-8000-00000000c0de",
             "summary":"the composer proof thread",
             "last_turn_at":"2026-09-14T12:00:00Z","turn_count":1}]}
JSON

# ── the driver ───────────────────────────────────────────────────────────────
# Runs the binary under a real pty, writes a SCRIPT of byte chunks into it with
# waits between them, and prints the rendered grid plus the DEC private modes it
# requested. The script is a python literal so a test can express exact bytes —
# `\x1b[200~…\x1b[201~`, `\x1b[27;2;13~`, an 800 KB paste — which is the whole
# point of testing at this layer.
DRIVE_PY="$CRATE_DIR/pty_drive.py"

# `into <script>` runs the script AFTER descending into the thread's composer.
# **MEASURED AGAINST THE FACE, NOT ASSUMED.** The root opens with the cursor on
# the `recent` BAND HEADER, where `→` is a no-op by design — so two presses of
# `→` land back on the root and every check below then reports on the main menu
# while claiming to report on the composer. `↓` reaches the thread's own row and
# `→` opens it, in ONE press, because a recent row IS a thread.
DESCEND='(b"\x1b[B", 0.6), (b"\x1b[C", 1.4),'
drive() { python3 "$DRIVE_PY" "${2:-100}" "${3:-24}" "$BIN" "$FAKEHOME" "$1"; }
into()  { drive "[$DESCEND $1]" "${2:-100}" "${3:-24}"; }
screen() { printf '%s' "$1" | sed -n 's/^SCREEN //p'; }
has()   { printf '%s' "$1" | grep -qF -- "$2"; }

build >/dev/null

# ── 0. we are actually in a composer ─────────────────────────────────────────
head_ "0. the descent — ↓ then → reaches a real composer"
# **THE ANCHOR IS THE BOX'S OWN PROMPT, NOT THE BAND'S LEGEND.** This asked for
# the words `composer` and `enter SENDS all`, which were the band's permanent
# teaching row until the operator said he did not need instructions on how to
# use the textarea and they were removed. An anchor on a LABEL is a check that
# goes stale in silence the moment the label is a design decision; the empty
# box's own placeholder is what a composer has whatever its border says.
OUT="$(into '')"
if has "$OUT" "type a message"; then
    ok "the composer band is up with the caret in it"
else
    bad "never reached the composer; the fixture or the descent is wrong"
    screen "$OUT" | sed -n '1,30p' | sed 's/^/        /'
fi
if has "$OUT" "?2004h"; then
    ok "bracketed paste is claimed (?2004h) — a paste can be ONE event"
else
    bad "?2004h is not claimed; a paste arrives as a keystroke storm"
fi
if has "$OUT" "?2026h"; then
    ok "synchronized output is used per frame (?2026h) — the growing band cannot tear"
else
    bad "?2026 was never written; the band tears as it grows"
fi

# ── 1. THE THREE NEWLINES, each arriving as its own bytes ────────────────────
head_ "1. the newline, three ways, with ZERO protocol negotiated"
# `\x1b\r` is alt-enter on every terminal. `\n` is ^J. Both must make a SECOND
# composer row appear; neither may send.
OUT="$(into '(b"one", 0.3), (b"\x1b\r", 0.3), (b"two", 0.6),')"
if has "$OUT" "one" && has "$OUT" "two" && ! has "$OUT" "line 1 of 1"; then
    ok "alt-enter (\\x1b\\r) inserted a newline"
else
    bad "alt-enter did not make a second line"
    screen "$OUT" | grep -n "composer" -A 4 | sed 's/^/        /'
fi
OUT="$(into '(b"one", 0.3), (b"\n", 0.3), (b"two", 0.6),')"
if has "$OUT" "one" && has "$OUT" "two"; then
    ok "^J (\\n) inserted a newline"
else
    bad "^J did not make a second line"
fi
# …and `\r` is the SEND, which on a dead link is refused BY NAME. That refusal
# is the proof the key reached the send path without a daemon being touched.
OUT="$(into '(b"a message", 0.3), (b"\r", 1.0),')"
if has "$OUT" "not connected"; then
    ok "enter (\\r) reached the SEND path — refused by name, with no daemon touched"
else
    bad "enter did not reach the send path"
    screen "$OUT" | tail -6 | sed 's/^/        /'
fi

# ── 2. IS-33, CLOSED: CSI 27;m;c~ IS A KEY NOW ───────────────────────────────
head_ "2. CSI 27;m;c~ — shift-enter, in the spelling his tmux actually sends"
# tmux with the DEFAULT `extended-keys-format xterm` answers modifyOtherKeys
# with `CSI 27;2;13~`. crossterm has no match arm for a leading 27, on 0.28.1
# and on master, so `parse_csi_special_key_code` errored and `Parser::advance`
# CLEARED THE BUFFER: the keystroke vanished with nothing reporting a loss, and
# `shift-enter` was byte-identical to `enter` — which SENDS.
#
# **THIS SECTION USED TO ASSERT THE DEFECT WAS PRESENT.** That was the honest
# shape for a tripwire over somebody else's bug while the bytes were
# unreachable. `0050` made them reachable — `src/vt.rs` owns the read and
# parses both dialects — so the assertion is INVERTED: `abcd` on one line is
# now the FAILURE, not the pass. Section 7d mutates the arm back out and
# requires this to go red.
OUT="$(into '(b"ab", 0.3), (b"\x1b[27;2;13~", 0.5), (b"cd", 0.6),')"
if has "$OUT" "ab" && has "$OUT" "cd" && ! has "$OUT" "abcd"; then
    ok "CSI 27;2;13~ (shift-enter, the xterm spelling) inserted a NEWLINE — IS-33 closed"
else
    bad "shift-enter is doing nothing again: IS-33 has regressed"
    screen "$OUT" | grep -n "composer" -A 4 | sed 's/^/        /'
fi
# …and it must NOT have reached the send path. "Inserted a newline" and "sent
# and cleared the box" can look similar on a grid, and a shift-enter that SENDS
# is the whole of the original defect.
if has "$OUT" "not connected" || has "$OUT" "sent"; then
    bad "SHIFT-ENTER SENT. That is IS-33 exactly."
    screen "$OUT" | tail -6 | sed 's/^/        /'
else
    ok "and it reached no send path at all"
fi
# The CSI-u spelling, which crossterm CAN parse, must work — this is the
# encoding tmux emits under `extended-keys-format csi-u`, and it is the whole
# reason that option is worth offering him.
OUT="$(into '(b"ab", 0.3), (b"\x1b[13;2u", 0.5), (b"cd", 0.6),')"
if has "$OUT" "ab" && has "$OUT" "cd" && ! has "$OUT" "abcd"; then
    ok "CSI 13;2u (shift-enter, csi-u spelling) IS parsed and inserts a newline"
else
    bad "the csi-u spelling of shift-enter did nothing"
fi
# A RELEASE must never act. Under the kitty protocol `:3` is a key release, and
# a face that acted on one would double every keystroke it saw.
OUT="$(into '(b"ab", 0.3), (b"\x1b[13;2:3u", 0.5), (b"cd", 0.6),')"
if has "$OUT" "abcd"; then
    ok "a key RELEASE (CSI 13;2:3u) does nothing at all"
else
    bad "a key release acted — every action would double"
fi

# ── 3. THE PASTE DEFECT, ON THE WIRE ─────────────────────────────────────────
head_ "3. a paste carrying its own terminator — the security-shaped one"
# The exact bytes the research reproduced. crossterm ends the paste at the FIRST
# `\x1b[201~` and delivers the rest as keystrokes, ending in a carriage return
# that would SEND. The batch rule folds the tail back into the paste, so what
# must be true is: the words are all in the box, and NOTHING was sent.
INJECT='(b"\x1b[200~hello\x1b[201~\x1b[201~ rm -rf\r", 1.2),'
OUT="$(into "$INJECT")"
if has "$OUT" "not connected" || has "$OUT" "sent"; then
    bad "A PASTE SENT A MESSAGE. That is his words going out half-finished."
    screen "$OUT" | tail -6 | sed 's/^/        /'
else
    ok "the paste did NOT send — no send path was reached at all"
fi
if has "$OUT" "hello rm -rf"; then
    ok "and the clipboard's tail was KEPT as text rather than dropped"
else
    bad "the tail was lost; a crossterm patch would have dropped it, and so did we"
    screen "$OUT" | grep -n "composer" -A 3 | sed 's/^/        /'
fi
# …and a REAL enter afterwards still sends, which is what makes the rule a
# defence rather than a lock.
OUT="$(into "$INJECT (b\"\\r\", 1.0),")"
if has "$OUT" "not connected"; then
    ok "a real enter AFTER the paste still reaches the send path"
else
    bad "the paste rule wedged the send path shut"
fi

# ── 4. NORMALIZATION ON THE WAY IN ───────────────────────────────────────────
head_ "4. what a paste may carry into the buffer, and what it may not"
OUT="$(into '(b"\x1b[200~l1\r\nl2\x00nul\x07bel\ttab\x1b[201~", 1.0),')"
if has "$OUT" "l1" && has "$OUT" "l2" && has "$OUT" "nul" && has "$OUT" "bel" && has "$OUT" "tab"; then
    ok "CRLF, NUL, BEL and TAB all survived as TEXT — nothing was silently dropped"
else
    bad "a pasted line was lost"
    screen "$OUT" | grep -n "composer" -A 5 | sed 's/^/        /'
fi
if has "$OUT" "line 1 of 2" || has "$OUT" "line 2 of 2" || ! has "$OUT" "l1l2"; then
    ok "CRLF became a real line break rather than a stray CR inside a line"
else
    bad "CRLF did not break the line"
fi

# ── 5. THE BIG PASTE, AND THE ONE THAT NEVER ENDS ────────────────────────────
head_ "5. an 800 KB paste, and a paste whose terminator never arrives"
python3 - "$WORK" <<'PY'
import sys
big = ("x" * 79 + "\n") * 10_000          # 10,000 lines, ~800 KB
open(sys.argv[1] + "/big.txt", "w").write(big)
PY
BIGSCRIPT="(b'\\x1b[200~' + open('$WORK/big.txt','rb').read() + b'\\x1b[201~', 3.0),"
OUT="$(into "$BIGSCRIPT" 100 24)"
if has "$OUT" "composer"; then
    ok "an 800 KB / 10,000-line paste did not kill the face"
else
    bad "the face died on a large paste"
fi
if has "$OUT" "of 10000" || has "$OUT" "line "; then
    ok "and the band reports where the caret is rather than trying to be 10,000 rows tall"
else
    note "no caret note on screen — the band may be short enough not to need one"
fi
# A paste whose terminator never arrives. The face must stay ALIVE and keep
# answering rather than wedging (pi#9119 is that bug in another tool); 7c
# proves what it does with the CONTENT.
#
# **THE `\x1b\x1b` THIS USED TO SEND IS GONE, and its removal is the finding.**
# The old script fed an ESC into the stall on the reasoning that it was inert —
# and it WAS, because crossterm was holding it in the same unreachable buffer
# as everything else. Since `0050` an ESC is a live key that means BACK, so it
# left the thread and this section reported "the face wedged" about a face that
# had simply obeyed him. The keystroke was only safe to send here while the
# defect was present.
OUT="$(into '(b"\x1b[200~unterminated paste", 1.6), (b"\x1b[201~", 0.5), (b"after", 0.8),')"
if has "$OUT" "composer"; then
    ok "an unterminated paste did not wedge the face — it kept drawing and kept answering"
else
    bad "the face wedged on an unterminated paste"
    screen "$OUT" | tail -8 | sed 's/^/        /'
fi
# The LATE terminator must not paste a second time. The idle gap already
# closed this paste, so the `\x1b[201~` that turns up afterwards is residue and
# has nothing to terminate.
if has "$OUT" "unterminated paste" && ! has "$OUT" "unterminated pasteunterminated paste"; then
    ok "a terminator arriving after the gap closed the paste is dropped, not replayed"
else
    bad "the late terminator duplicated or lost the paste"
    screen "$OUT" | grep -n "composer" -A 4 | sed 's/^/        /'
fi

# ── 6. A RESIZE MID-EDIT ─────────────────────────────────────────────────────
head_ "6. a resize while a wrapped draft is on screen"
# The wrap is recomputed from the width the frame was handed, so a resize must
# re-wrap rather than leave the caret where the old width put it. If the caret's
# column were STORED this is where it would show.
OUT="$(python3 - "$WORK" "$BIN" "$FAKEHOME" <<'PY'
import os, sys, subprocess, pty, select, time, termios, fcntl, struct, re
WORK, BIN, HOME = sys.argv[1], sys.argv[2], sys.argv[3]
pid, fd = pty.fork()
if pid == 0:
    env = dict(os.environ); env["TERM"] = "xterm-256color"; env["HOME"] = HOME
    # The variables the face READS (R7.S5; pinned by `tests/drive_isolation.rs`).
    env["TUI_THREADS_CHASSIS_PORT"] = "1"; env["TUI_THREADS_PORT"] = "1"
    env["AUI_V2_URL"] = "ws://127.0.0.1:1/"; env["CAULDRON_BIN"] = "/usr/bin/false"
    env["CAULDRON_OPENER"] = "none"; env["CAULDRON_TYPE_ONE"] = "off"
    env["CAULDRON_HOME"] = os.path.join(WORK, "chome")
    env.pop("TMUX", None)
    os.execve(BIN, [BIN], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
buf = b""
def drain(s):
    global buf
    end = time.time() + s
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try: c = os.read(fd, 1 << 20)
            except OSError: return
            if not c: return
            buf += c
drain(1.2)
for k, w in [(b"\x1b[B", 0.6), (b"\x1b[C", 1.4),
             (b"the quick brown fox jumps over the lazy dog and keeps going for a while", 0.8)]:
    os.write(fd, k); drain(w)
# NARROW it hard, mid-edit.
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 46, 0, 0))
os.kill(pid, 28)   # SIGWINCH
drain(1.0)
os.write(fd, b"!"); drain(0.8)
os.write(fd, b"\x03"); drain(0.6)
try: os.kill(pid, 9); os.waitpid(pid, 0)
except Exception: pass
grid = [[" "] * 46 for _ in range(24)]
row = col = i = 0
while i < len(buf):
    b = buf[i]
    if b == 0x1B:
        m = re.match(rb"\x1b\[([0-9;?]*)([A-Za-z])", buf[i:])
        if m:
            p, f = m.group(1), m.group(2)
            if f == b"H":
                q = p.split(b";"); row = (int(q[0]) - 1) if q and q[0] else 0
                col = (int(q[1]) - 1) if len(q) > 1 and q[1] else 0
            elif f == b"J" and p in (b"2", b""):
                grid = [[" "] * 46 for _ in range(24)]
            i += m.end(); continue
        i += 1; continue
    if b == 0x0D: col = 0; i += 1; continue
    if b == 0x0A: row += 1; col = 0; i += 1; continue
    ln = 4 if b >= 0xF0 else 3 if b >= 0xE0 else 2 if b >= 0xC0 else 1
    try: ch = buf[i:i+ln].decode("utf-8")
    except UnicodeDecodeError: i += 1; continue
    if 0 <= row < 24 and 0 <= col < 46: grid[row][col] = ch
    col += 1; i += ln
for r in grid: print("SCREEN " + "".join(r).rstrip())
PY
)"
# **THE WITNESS IS THE HEADER'S LINK WORD, NOT THE WORD `composer`.** This
# asked for `composer` and passed on the FIXTURE THREAD'S TITLE — "the composer
# proof thread" — so it would have reported a live face over a dead one. A
# witness has to be keyed on something the thing under test cannot move; the
# link word is drawn by the header whatever the composer's wrap is doing.
if has "$OUT" "disconnected" || has "$OUT" "connecting"; then
    ok "the face survived a hard narrow mid-edit and kept drawing"
else
    bad "the face died or stopped drawing on a resize mid-edit"
fi
if has "$OUT" "dog and"; then
    ok "the draft RE-WRAPPED to the new width — the caret was not left at a stale column"
else
    bad "the draft did not re-wrap after the resize"
    screen "$OUT" | tail -8 | sed 's/^/        /'
fi

# ── 7. FORCED FAILURE: break the paste rule and require the suite to catch it ─
head_ "7. FORCED — remove the paste rule, and section 3 must go red"
python3 - "$INPUT_RS" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
# The mutation: make `defuse` a no-op, which is exactly the state the face was
# in before this round and exactly what a crossterm patch alone would leave.
s = s.replace("pub fn defuse(batch: Vec<Input>) -> Vec<Input> {",
              "pub fn defuse(batch: Vec<Input>) -> Vec<Input> { return batch;\n#[allow(unreachable_code)] {")
s = s.replace("""    out.push(Input::Paste(text));
    out.extend(tail);
    out
}""", """    out.push(Input::Paste(text));
    out.extend(tail);
    out
}}""")
open(p, "w").write(s)
PY
build >/dev/null
OUT="$(into "$INJECT")"
if has "$OUT" "not connected" || has "$OUT" "sent"; then
    ok "with the rule removed the paste DOES send — section 3 is discriminating"
else
    bad "the mutant still did not send: section 3 proves nothing"
    screen "$OUT" | tail -6 | sed 's/^/        /'
fi
cp "$BACKUP/input.rs" "$INPUT_RS"
build >/dev/null
OUT="$(into "$INJECT")"
if has "$OUT" "not connected" || has "$OUT" "sent"; then
    bad "the source did not restore cleanly — check git status"
else
    ok "restored: the real face does not send on that paste"
fi

# ── 7a. BOTH SPELLINGS OF BACKSPACE ─────────────────────────────────────────
head_ "7a. backspace, in both of the bytes a terminal may send for it"
# FOUND BY HAND. `\x7f` (DEL) is what his terminal's erase key sends and
# crossterm turns it into `Backspace`. `\x08` (`^H`) is what a terminal with
# `stty erase ^H` sends, and crossterm turns THAT into `Ctrl('h')` — which was
# bound to nothing, so backspace was a dead key on such a terminal. Both are
# asserted here because only one of them was ever going to be typed by accident.
OUT="$(into '(b"abcd", 0.4), (b"\x7f", 0.5),')"
if has "$OUT" "abc" && ! has "$OUT" "abcd"; then
    ok "\\x7f (DEL) deletes a character"
else
    bad "\\x7f did not delete"
    screen "$OUT" | grep -n "composer" -A 3 | sed 's/^/        /'
fi
OUT="$(into '(b"abcd", 0.4), (b"\x08", 0.5),')"
if has "$OUT" "abc" && ! has "$OUT" "abcd"; then
    ok "\\x08 (^H) deletes a character too — no terminal has a dead backspace"
else
    bad "^H is a DEAD KEY: a terminal with stty erase ^H cannot backspace"
    screen "$OUT" | grep -n "composer" -A 3 | sed 's/^/        /'
fi

# ── 7b. ^S MUST NOT FREEZE HIS TERMINAL ─────────────────────────────────────
head_ "7b. ^S and ^Q — XON/XOFF, which would freeze the screen beneath us"
# `Ctrl+S` is XON/XOFF flow control at the TERMIOS layer, and if raw mode does
# not clear IXON it stops the terminal's OUTPUT dead — a face that appears to
# have hung, with no key that brings it back except `^Q`, which he has no reason
# to guess. `cfmakeraw` clears it and crossterm does the equivalent, but the
# research asks for this to be ASSERTED rather than assumed, and it is the
# cheapest assertion in the whole suite: type, press ^S, type again, and look.
OUT="$(into '(b"before", 0.3), (b"\x13", 0.4), (b"after", 0.8),')"
if has "$OUT" "beforeafter"; then
    ok "^S did not freeze the output — IXON is clear and the face kept drawing"
else
    bad "^S FROZE THE SCREEN: raw mode did not clear IXON, and he has no way back"
    screen "$OUT" | grep -n "composer" -A 3 | sed 's/^/        /'
fi
# …and ^Q likewise, which is the other half of the pair.
#
# **RE-KEYED IN ROUND SIX, AND THE PROPERTY IS UNCHANGED — ONLY ITS SIGNATURE.**
# This asserted that typing, pressing ^Q, and typing again left BOTH words in
# the box: ^Q inert was the evidence that XON/XOFF never reached the TERMIOS
# layer. `0134` then bound ^Q to QueueMessage (it replaces the `opt-q` that
# never arrived on his keyboard), so ^Q now QUEUES the first word and the box
# legitimately holds only the second.
#
# The hazard this section exists for is UNCHANGED and still covered: if IXON
# were set, ^S would stop the terminal's output dead and nothing typed after it
# would draw at all. The ^S half above still proves that directly. For ^Q the
# evidence is now STRONGER than inertness — a keystroke that performs its bound
# act has demonstrably reached the APPLICATION rather than being eaten by the
# tty, which is the thing this check is really about.
#
# So: the face must still be DRAWING after ^Q, and the first word must have gone
# somewhere rather than vanished. A frozen terminal fails both.
OUT="$(into '(b"before", 0.3), (b"\x11", 0.4), (b"after", 0.8),')"
if has "$OUT" "after"; then
    ok "^Q reached the app and the face kept drawing — neither half of XON/XOFF is swallowed by the terminal"
else
    bad "^Q froze the output or was eaten by the tty; the flow-control pair is not fully cleared"
fi

# ── 7c. IS-38, CLOSED: AN UNTERMINATED PASTE ─────────────────────────────────
head_ "7c. a paste with no terminator — the idle gap, and the keyboard after it"
# **THIS SECTION USED TO BE A MEASUREMENT AND IS NOW A PROOF.** It read:
# "crossterm holds the bytes in its own buffer and there is no API that reaches
# them — so the honest thing is to MEASURE the cost". The cost it measured was
# a DEAF KEYBOARD: every keystroke after an unterminated paste was held by
# crossterm until a terminator happened to arrive.
#
# `0050` owns the read. His rule: "if you detect raw characters after the paste
# starts, and then all of a sudden there's a gap of 1s since the last
# character, it's probably done." So the content must LAND, and the keyboard
# must be live behind it. The 1.6s wait is the 1s gap with headroom for a
# debug build.
OUT="$(into '(b"typed", 0.4), (b"\x1b[200~swallowed", 1.6), (b"more typing", 0.8),')"
if has "$OUT" "typed"; then
    ok "what was typed BEFORE the unterminated paste is still on screen"
else
    bad "the face lost text it had already accepted"
fi
if has "$OUT" "swallowed"; then
    ok "IS-38: the unterminated paste's content LANDED on the 1s idle gap"
else
    bad "the content of an unterminated paste was lost — that is his clipboard, gone"
    screen "$OUT" | grep -n "composer" -A 4 | sed 's/^/        /'
fi
if has "$OUT" "more typing"; then
    ok "IS-38: THE KEYBOARD IS ALIVE after an unterminated paste — the deafness is gone"
else
    bad "keystrokes after an unterminated paste are still being held: IS-38 is back"
    screen "$OUT" | grep -n "composer" -A 4 | sed 's/^/        /'
fi
# …and nothing in it may have SENT. The gap-closed paste goes through the same
# door a normal one does, so its trailing bytes are text, not an `enter`.
OUT="$(into '(b"\x1b[200~line one\rline two", 1.6),')"
if has "$OUT" "not connected" || has "$OUT" "sent"; then
    bad "a gap-closed paste SENT — the CR in it was treated as his ENTER"
    screen "$OUT" | tail -6 | sed 's/^/        /'
else
    ok "a CR inside a gap-closed paste is a newline and reached no send path"
fi

# ── 7d. THE PASTE THAT SAYS NOTHING — the timeout, and the notice ────────────
head_ "7d. a paste that opens and never speaks — the timeout says so out loud"
# The other half of IS-38, and it is a DIFFERENT case from the idle gap: an
# empty run of silence after `CSI 200~` cannot be read as a completed empty
# paste, so it fails by name instead. His instruction: "Show one when paste is
# in progress, and if it fails via timeout then say so."
#
# 1.4s in is INSIDE the 3s timeout, so the in-progress notice must be up; 4.0s
# in is past it, so the failure notice must have replaced it.
OUT="$(into '(b"\x1b[200~", 1.4),')"
if has "$OUT" "paste in progress"; then
    ok "the info area says a paste is in progress while one is open"
else
    bad "a stalled paste is invisible — the face looks frozen with no explanation"
    screen "$OUT" | tail -4 | sed 's/^/        /'
fi
OUT="$(into '(b"\x1b[200~", 4.0), (b"alive", 0.8),')"
if has "$OUT" "paste failed"; then
    ok "and when it times out the info area SAYS SO — 'paste failed'"
else
    bad "a timed-out paste failed silently"
    screen "$OUT" | tail -4 | sed 's/^/        /'
fi
if has "$OUT" "alive"; then
    ok "and the keyboard came back after the timeout"
else
    bad "the keyboard is still deaf after the paste timed out"
fi
# A NORMAL paste must NOT show the in-progress notice: it opens and closes
# inside one batch, and a notice that flashes on every paste is noise.
OUT="$(into '(b"\x1b[200~quick\x1b[201~", 1.0),')"
if has "$OUT" "quick" && ! has "$OUT" "paste in progress"; then
    ok "a paste that completes at once shows NO notice — the batch absorbs it"
else
    bad "an ordinary paste flashed the in-progress notice"
    screen "$OUT" | tail -4 | sed 's/^/        /'
fi

# ── 7e. FORCED: break the new layer, and 2 and 7c must go red ────────────────
head_ "7e. FORCED — remove the IS-33 arm and the IS-38 gap, watch them fail"
# `core:writing-tools`: a green you have not forced to fail is not evidence.
# Sections 2 and 7c assert two behaviours that did not exist an hour ago, so
# each is mutated OUT of `src/vt.rs` and the section is required to go red.
python3 - "$VT_RS" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
# (a) IS-33: make the `27` arm fall through, which is exactly crossterm's state.
s = s.replace("    if first == 27 {", "    if false {", 1)
# (b) IS-38: make the idle gap never fire, which is exactly crossterm's state.
s = s.replace("if !p.text.is_empty() && quiet >= PASTE_IDLE {",
              "if false && !p.text.is_empty() && quiet >= PASTE_IDLE {", 1)
open(p, "w").write(s)
PY
build >/dev/null
OUT="$(into '(b"ab", 0.3), (b"\x1b[27;2;13~", 0.5), (b"cd", 0.6),')"
if has "$OUT" "abcd"; then
    ok "with the 27-arm removed shift-enter vanishes again — section 2 is discriminating"
else
    bad "the mutant still handled shift-enter: section 2 proves nothing"
    screen "$OUT" | grep -n "composer" -A 3 | sed 's/^/        /'
fi
OUT="$(into '(b"typed", 0.4), (b"\x1b[200~swallowed", 1.6), (b"more typing", 0.8),')"
if has "$OUT" "more typing"; then
    bad "the mutant's keyboard still worked: section 7c proves nothing"
else
    ok "with the idle gap removed the keyboard goes deaf again — section 7c is discriminating"
fi
cp "$BACKUP/vt.rs" "$VT_RS"
build >/dev/null
OUT="$(into '(b"ab", 0.3), (b"\x1b[27;2;13~", 0.5), (b"cd", 0.6),')"
if has "$OUT" "abcd"; then
    bad "the source did not restore cleanly — check git status"
else
    ok "restored: the real face parses shift-enter again"
fi

# ── 8. what a family emoji ACTUALLY occupies on this terminal ────────────────
head_ "8. the width disagreement, MEASURED rather than argued"
# `unicode-width` says a ZWJ family emoji is 2 columns and `cluster_width`
# agrees. What the TERMINAL believes is a separate question with no protocol to
# ask it (Mode 2027 is the answer and tmux does not implement it), so this
# section reports rather than asserts: it types the emoji, then an `X`, and
# prints the row so a human can see where the X landed.
OUT="$(into '(b"[", 0.3), (b"\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7\xe2\x80\x8d\xf0\x9f\x91\xa6", 0.5), (b"]X", 0.8),')"
ROW="$(screen "$OUT" | grep -F "[" | grep -F "]X" | head -1)"
if [ -n "$ROW" ]; then
    ok "the composer drew the family emoji and kept drawing after it"
    note "row as the face emitted it: ${ROW}"
    note "the face measures this cluster at 2 columns; what the TERMINAL does with it"
    note "is unobservable from inside — bounded, not closed. See 0042's risks."
else
    bad "the emoji row is not on screen at all"
    screen "$OUT" | grep -n "composer" -A 3 | sed 's/^/        /'
fi

# ── 9. the draft journal, on disk, with its modes ────────────────────────────
head_ "9. the draft journal — written 0600, in a 0700 directory"
OUT="$(into '(b"a draft worth keeping", 0.3), (b"", 1.2),')"
DRAFT="$(ls "$FAKEHOME/.tui/draft" 2>/dev/null | head -1)"
if [ -n "$DRAFT" ]; then
    ok "a draft was journalled after the debounce"
    DMODE="$(stat -f '%Lp' "$FAKEHOME/.tui/draft/$DRAFT")"
    DIRMODE="$(stat -f '%Lp' "$FAKEHOME/.tui/draft")"
    [ "$DMODE" = "600" ] && ok "the file is 0600" || bad "the draft file is $DMODE, not 0600"
    [ "$DIRMODE" = "700" ] && ok "the directory is 0700" || bad "the draft directory is $DIRMODE, not 0700"
    grep -qF "a draft worth keeping" "$FAKEHOME/.tui/draft/$DRAFT" \
        && ok "and it holds what he typed" \
        || bad "the journal does not hold the draft"
    # …and REOPENING offers it back rather than re-typing it.
    OUT="$(into '(b"", 0.5),')"
    if has "$OUT" "was recovered"; then
        ok "reopening OFFERS the draft rather than silently re-inserting it"
    else
        bad "the recovered draft was not offered"
        screen "$OUT" | grep -n "composer" -A 3 | sed 's/^/        /'
    fi
else
    bad "nothing was journalled; the debounce or the path is wrong"
    ls -la "$FAKEHOME/.tui" 2>&1 | sed 's/^/        /'
fi

printf '\n'
if [ "$FAILURES" -eq 0 ]; then
    printf '\033[32mALL PROVEN\033[0m — the composer answers the terminal correctly.\n'
    exit 0
fi
printf '\033[31m%d FAILURE(S)\033[0m\n' "$FAILURES"
exit 1
