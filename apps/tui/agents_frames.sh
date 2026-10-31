#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  tui SUBAGENT TREE — THE PTY LAYER. Everything here is a fact only a terminal
#  can answer: what the face actually PAINTS when a thread has subagents in it,
#  at his own pane size and at a narrow one, and what happens to the message box
#  when the thing above it is a subagent nothing can be sent to.
#
#  > *I just want to see the subagents, and their subagents. It looks like a
#  > file tree… I don't want to see them in the chat area, it takes up 90% of
#  > the chat area… we should be able to navigate to those subagents in the tree
#  > view below, and if we click right or enter on them we show their chat
#  > history instead of the parent. Basically the text area above should be
#  > whichever one we've selected, so the parent needs to be an item in that
#  > list.*
#
#  The unit tests prove the projection and the goldens prove the layout; this
#  proves that a real face, driven by real bytes through a real pty, puts it on
#  the screen. `core:writing-tools`: a green you have not forced to fail is not
#  evidence — §6 MUTATES the source to remove the band and requires this suite
#  to notice, and §7 restores it and re-proves the check bites.
#
#  WHAT IT TOUCHES: nothing outside this crate. The daemon it talks to is
#  `examples/fake_daemon`, bound to an ephemeral loopback port, seeded with a
#  subagent tail by `TUI_FAKE_SUBAGENTS`. `pty_drive.py` hands the face that
#  port through `TUI_THREADS_CHASSIS_PORT` (the variable the seam reads) and
#  refuses a live one; his own daemon is on 3653. `$HOME` is redirected for
#  the whole run.
#
#  Usage:  ./agents_frames.sh
#  Exit:   0 everything proven; 1 otherwise.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
. "$CRATE_DIR/conformance_target.sh"
. "$CRATE_DIR/conformance_mirror.sh"

BIN="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/tui"
FAKE="${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/examples/fake_daemon"
WORK="$(mktemp -d)"
RENDER_RS="$CRATE_DIR/../../libs/tui-render/src/render.rs"
BACKUP="$WORK/backup"
FAILURES=0
DAEMON_PID=""

mkdir -p "$BACKUP"
cp "$RENDER_RS" "$BACKUP/render.rs"
cleanup() {
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
    [ -f "$BACKUP/render.rs" ] && cp "$BACKUP/render.rs" "$RENDER_RS"
    rm -rf "$WORK"
}
trap 'cleanup; conformance_target_release' EXIT INT TERM

ok()    { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
bad()   { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
note()  { printf '        %s\n' "$1"; }
head_() { printf '\n\033[1m%s\033[0m\n' "$1"; }

build() { (cd "$REPO_ROOT" && cargo build -p tui --example fake_daemon 2>&1 | tail -3 >/dev/null
           cd "$REPO_ROOT" && cargo build -p tui 2>&1 | tail -3); }

FAKEHOME="$WORK/home"
mkdir -p "$FAKEHOME/minds/proof/.mind"
cat > "$FAKEHOME/minds/proof/.mind/threads.json" <<'JSON'
{"threads":[{"id":"00000000-0000-4000-8000-00000000c0de",
             "session_id":"00000000-0000-4000-8000-00000000c0de",
             "summary":"the subagent tree proof thread",
             "last_turn_at":"2026-09-14T12:00:00Z","turn_count":1}]}
JSON

PORT=8823
start_daemon() {
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
    TUI_FAKE_SUBAGENTS=1 "$FAKE" "$PORT" >"$WORK/daemon.log" 2>&1 &
    DAEMON_PID=$!
    sleep 1.2
}

DRIVE_PY="$CRATE_DIR/pty_drive.py"
# The root opens with the cursor on the `recent` BAND HEADER, where `→` is a
# no-op by design; `↓` reaches the thread's own row and `→` opens it. Measured
# against the face rather than assumed — an unmeasured descent is a suite that
# tests the root menu and reports on the thread.
DESCEND='(b"\x1b[B", 0.8), (b"\x1b[C", 3.0),'
into()  { TUI_FAKE_PID="$DAEMON_PID" python3 "$DRIVE_PY" "${2:-97}" "${3:-35}" "$BIN" "$FAKEHOME" \
              "[$DESCEND $1]" "ws://127.0.0.1:$PORT/ws"; }
screen() { printf '%s' "$1" | sed -n 's/^SCREEN //p'; }
has()   { printf '%s' "$1" | grep -qF -- "$2"; }

build >/dev/null
start_daemon

# ── 1 · THE BAND IS ON THE GLASS AT HIS OWN PANE SIZE ────────────────────────
head_ "1 · 97×35 — the tree is painted below the box, parent first"
OUT="$(into '(b"", 1.2),')"
if has "$OUT" "agents · "; then
    ok "the subagent band is on the screen"
else
    bad "no subagent band at 97×35 — the fixture, the daemon or the band is wrong"
    screen "$OUT" | sed -n '1,40p' | sed 's/^/        /'
fi
for want in "this thread" "running" "Explore" "core:builder"; do
    if has "$OUT" "$want"; then ok "the band shows '$want'"; else bad "the band is missing '$want'"; fi
done
if has "$OUT" "31k tok"; then ok "the measured token figure is painted"; else bad "no token figure on the glass"; fi
if has "$OUT" "└" || has "$OUT" "├"; then
    ok "it is drawn as a FILE TREE — the rails are painted"
else
    bad "no rails: this is a flat list, not the file tree he asked for"
fi

# ── 2 · THE SUBAGENTS ARE OUT OF THE CHAT ────────────────────────────────────
head_ "2 · the chat area holds none of the subagents' own rows"
if has "$OUT" "The projection is in store.rs."; then
    bad "a subagent's own reply is still in his chat area"
else
    ok "a subagent's reply is NOT in his chat — it is in the subagent's own"
fi
# **THE GESTURE MUST BE ON THE TILE'S OWN ROW, not merely somewhere on the
# screen.** A bare screen-wide grep for the string passes on any row that
# happens to carry it — a rule, a banner, a help line — and would report the
# redundancy present while the tile itself said nothing about drilling. So the
# check is a SINGLE ROW that both names a subagent and offers the gesture,
# which is what `0102` says the tile is.
if screen "$OUT" | grep -qF -- "agent · " && \
   screen "$OUT" | grep -F -- "opens its chat" | grep -qF -- "agent · "; then
    ok "ONE info tile per subagent remains in the chat, as he authorised"
else
    bad "no tile in the chat: the authorised redundancy is missing"
    screen "$OUT" | grep -n -- "agent" | sed 's/^/        /'
fi
if has "$OUT" "scout the raw list before we cut it"; then
    ok "HIS OWN words are still in his chat"
else
    bad "his own message left the chat with the subagents'"
fi

# ── 3 · NAVIGABLE: ↓ ↓ → OPENS A SUBAGENT'S OWN HISTORY ──────────────────────
head_ "3 · ↓ into the band, ↓ onto a subagent, → opens its chat"
DRILL='(b"\x1b[B", 0.5), (b"\x1b[B", 0.5), (b"\x1b[C", 1.2),'
OUT2="$(into "$DRILL")"
if has "$OUT2" "The projection is in store.rs."; then
    ok "→ on a subagent row replaced the parent's chat with the subagent's"
else
    bad "→ did not open the subagent's chat history"
    screen "$OUT2" | sed -n '1,40p' | sed 's/^/        /'
fi
if has "$OUT2" "read the raw list projection"; then
    ok "the DIRECT INVOCATION is the first turn of its chat"
else
    bad "the subagent's chat starts mid-sentence — no invocation"
fi
if has "$OUT2" "scout the raw list before we cut it"; then
    bad "the PARENT's message is still shown under the subagent"
else
    ok "the parent's own rows are not in the subagent's chat"
fi

# ── 4 · THE COMPOSER SAYS IT CANNOT SEND ─────────────────────────────────────
head_ "4 · the box against a claude-code subagent refuses rather than looking live"
if has "$OUT2" "nothing can be sent to it"; then
    ok "the band says why there is no box"
else
    bad "the composer looks live against a subagent with no inbox"
fi
if has "$OUT2" "your draft is kept"; then
    ok "it says what happened to his draft"
else
    bad "it does not say what happened to his draft"
fi
# …and typing into it neither types nor is silent.
TYPE="$DRILL"'(b"hello", 1.0),'
OUT3="$(into "$TYPE")"
if has "$OUT3" "nothing can be sent to"; then
    ok "a printable aimed at a subagent answers with the notice"
else
    bad "a key aimed at a subagent was swallowed in silence"
fi

# ── 5 · NARROW ───────────────────────────────────────────────────────────────
head_ "5 · 60 columns — the figures drop right to left, the band survives"
OUTN="$(into '(b"", 1.2),' 60 35)"
if has "$OUTN" "agents · "; then ok "the band is still drawn at 60"; else bad "the band vanished at 60 columns"; fi
if has "$OUTN" "calls"; then
    bad "60 columns still tries to print every figure"
else
    ok "the call count is dropped at 60 rather than cropped"
fi

# ── 6 · FORCED FAILURE — remove the band, the suite must notice ──────────────
head_ "6 · FORCED: tree_height returns 0, and §1 must go red"
python3 - "$RENDER_RS" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
old = "pub fn tree_height(t: &AgentTree, room: usize) -> usize {\n    if t.rows.len() < 2 {"
assert s.count(old) == 1, "ANCHOR-MISSING tree_height"
open(p, "w").write(s.replace(old, "pub fn tree_height(t: &AgentTree, room: usize) -> usize {\n    if true { return 0; }\n    if t.rows.len() < 2 {"))
PY
if [ $? -ne 0 ]; then bad "ANCHOR-MISSING tree_height — this suite stopped checking what it claims to"; fi
build >/dev/null
OUTM="$(into '(b"", 1.2),')"
if has "$OUTM" "agents · "; then
    bad "the band is still on the screen with tree_height stubbed to 0 — §1 proves nothing"
else
    ok "the band is gone when the code that draws it is removed — §1 bites"
fi

# ── 7 · RESTORE and re-prove ─────────────────────────────────────────────────
head_ "7 · the source is restored byte-for-byte and the band comes back"
cp "$BACKUP/render.rs" "$RENDER_RS"
if cmp -s "$BACKUP/render.rs" "$RENDER_RS"; then
    ok "render.rs restored byte-for-byte"
else
    bad "render.rs was NOT restored"
fi
build >/dev/null
OUTR="$(into '(b"", 1.2),')"
if has "$OUTR" "agents · "; then
    ok "the band is back"
else
    bad "the band did not come back after the restore"
fi

printf '\n\033[1m── subagent tree: %d failed ──\033[0m\n' "$FAILURES"
[ "$FAILURES" -eq 0 ] || exit 1
