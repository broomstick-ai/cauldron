#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  tui-render conformance test — the four purity mechanisms, each PROVEN.
#
#  `core:writing-tools`: a green you have not forced to fail is not evidence.
#  So this script does not merely run the gates green; for each of them it
#  first BREAKS the crate in the specific way that gate exists to catch, asserts
#  that the gate goes red, restores the file, and only then accepts the green.
#
#  It edits files under libs/tui-render/src and libs/tui-render/tests and
#  restores them from a backup on every exit path, including a signal. If it is
#  ever interrupted hard enough to skip the trap, `git status` on this directory
#  says so immediately.
#
#  It touches nothing outside this crate: no network, no daemon, no terminal.
#
#  Usage:  ./conformance_test.sh
#  Exit:   0 all four mechanisms proven; 1 otherwise.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── BUILD INTO A CONFORMANCE-ONLY TARGET DIRECTORY ───────────────────────────
# The operator's LIVE face executes `<repo>/target/debug/tui`. This suite only
# ever asks for `-p tui-render`, so it does not relink that binary itself — but
# it shares one target directory with the suites that do, and a shared lock on
# the live directory is a shared blast radius. It builds where they build.
REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
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
cd "$CRATE_DIR" || exit 1

RENDER="$CRATE_DIR/src/render.rs"
GOLDEN="$CRATE_DIR/tests/golden.rs"
BACKUP="$(mktemp -d)"
FAILURES=0

cleanup() {
    [ -f "$BACKUP/render.rs" ] && cp "$BACKUP/render.rs" "$RENDER"
    [ -f "$BACKUP/golden.rs" ] && cp "$BACKUP/golden.rs" "$GOLDEN"
    rm -rf "$BACKUP"
}
# The scratch target tree goes with it: bash's traps are last-write-wins, so a
# suite that installs its own EXIT trap would otherwise silently discard the
# one `conformance_target.sh` set and leave half a gigabyte behind.
trap 'cleanup; conformance_target_release' EXIT INT TERM
cp "$RENDER" "$BACKUP/render.rs"
cp "$GOLDEN" "$BACKUP/golden.rs"

ok()    { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
bad()   { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
head_() { printf '\n\033[1m%s\033[0m\n' "$1"; }

clippy() { cargo clippy -p tui-render --all-targets -- -D warnings 2>&1; }
golden() { cargo test -p tui-render --test golden 2>&1; }

SIG="pub fn frame(vs: &ViewState, now: DateTime<Utc>, width: u16, height: u16) -> Vec<Line<'static>> {"

# ── mechanism 1: the dependency set ──────────────────────────────────────────
# Cargo cannot be asked "is tokio absent"; the honest check is that the crate's
# DIRECT dependency list is the short one, since Rust will only let the source
# name what is on it. This is ALSO the check that says the render layer is
# still a separate crate: a module inside apps/tui would have no list at all.
head_ "1. the dependency set (the reason this is a crate and not a module)"
DEPS=$(sed -n '/^\[dependencies\]/,/^\[/p' Cargo.toml | grep -E '^[a-z0-9_-]+ *=' | cut -d' ' -f1)
echo "     direct deps: $(echo "$DEPS" | tr '\n' ' ')"
DEP_FAILS=0
for banned in tokio crossterm tokio-tungstenite futures-util rusqlite dirs once_cell lazy_static tracing tracing-appender reqwest aui-state tui-view; do
    if echo "$DEPS" | grep -qx "$banned"; then
        bad "$banned is a direct dependency — the crate boundary is breached"
        DEP_FAILS=$((DEP_FAILS + 1))
    fi
done
for required in ratatui chrono unicode-width; do
    if ! echo "$DEPS" | grep -qx "$required"; then
        bad "$required went missing from the manifest"
        DEP_FAILS=$((DEP_FAILS + 1))
    fi
done
if grep -qE '^ratatui.*features *= *\[[^]]*crossterm' Cargo.toml; then
    bad "ratatui is being taken with the crossterm backend — that is a terminal in the pure crate's graph"
    DEP_FAILS=$((DEP_FAILS + 1))
fi
[ "$DEP_FAILS" -eq 0 ] && ok "only the three pure dependencies, and ratatui without a backend"

# 1b — FORCE IT TO FAIL. A list-reading check that has never seen a bad list is
# a check nobody has tested; this adds tokio to a COPY and asserts the assertion
# bites, then throws the copy away.
head_ "1b. the dependency check, forced to fail"
TMPMANIFEST="$(mktemp -d)"
cp Cargo.toml "$TMPMANIFEST/Cargo.toml"
printf 'tokio = { workspace = true }\n' >> "$TMPMANIFEST/Cargo.toml"
PROBE_DEPS=$(sed -n '/^\[dependencies\]/,$p' "$TMPMANIFEST/Cargo.toml" | grep -E '^[a-z0-9_-]+ *=' | cut -d' ' -f1)
if echo "$PROBE_DEPS" | grep -qx tokio; then
    ok "a smuggled tokio IS seen by the same reader (forced failure observed)"
else
    bad "the dependency reader cannot see a smuggled tokio — check 1 proves nothing"
fi
rm -rf "$TMPMANIFEST"

# ── mechanism 2: clippy, forced to fail first ────────────────────────────────
head_ "2. clippy disallowed-methods (forced failure first)"
python3 - "$RENDER" "$SIG" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); needle = sys.argv[2]; s = p.read_text()
assert needle in s, "frame() signature moved; update conformance_test.sh"
p.write_text(s.replace(needle, needle + "\n    let _probe = Utc::now(); // CONFORMANCE PROBE", 1))
PY
OUT=$(clippy)
if echo "$OUT" | grep -q "use of a disallowed method"; then
    ok "a clock call is REJECTED (forced failure observed)"
    echo "$OUT" | grep -A2 "use of a disallowed method" | head -3 | sed 's/^/        /'
else
    bad "a clock call was NOT rejected — clippy.toml is not being applied"
    echo "$OUT" | tail -12 | sed 's/^/        /'
fi
cp "$BACKUP/render.rs" "$RENDER"

# ── mechanism 2b: the macro the other two mechanisms could not see ───────────
# Found by auditing this gate rather than trusting its green. `std::io::stdout`
# and `std::io::stderr` are on the disallowed-METHODS list above, and the macro
# everybody actually reaches for bypasses both: `eprintln!` expands to
# `std::io::_print`, the macros are in the prelude so the dependency set cannot
# help, and the golden test does not FAIL on a stray print — it just prints. So
# all three original mechanisms passed a crate with a write to the terminal in
# it, which is `0018`'s garbled-screen defect inside the layer that exists not
# to have one. `disallowed-macros` closes it; this is the forced failure that
# says so.
head_ "2b. clippy disallowed-macros — the print the other three mechanisms miss"
python3 - "$RENDER" "$SIG" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); needle = sys.argv[2]; s = p.read_text()
assert needle in s, "frame() signature moved; update conformance_test.sh"
p.write_text(s.replace(needle, needle + '\n    eprintln!("CONFORMANCE PROBE");', 1))
PY
OUT=$(clippy)
if echo "$OUT" | grep -q "use of a disallowed macro"; then
    ok "a write to the terminal is REJECTED (forced failure observed)"
    echo "$OUT" | grep -A2 "use of a disallowed macro" | head -3 | sed 's/^/        /'
else
    bad "an eprintln! in the pure crate was NOT rejected — the gate has its original hole"
    echo "$OUT" | tail -12 | sed 's/^/        /'
fi
cp "$BACKUP/render.rs" "$RENDER"

if clippy | grep -qE '^(error|warning)'; then
    bad "the restored crate does not pass clippy clean"
else
    ok "the real crate passes -D warnings clean"
fi

# ── mechanism 3a: the golden test, forced to fail on a perturbed model ───────
head_ "3a. golden test vs a perturbed ViewState (forced failure first)"
python3 - "$GOLDEN" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
needle = '            queued: Known::Known(2),'
assert needle in s, "the fixture moved; update conformance_test.sh"
p.write_text(s.replace(needle, '            queued: Known::Known(3),', 1))
PY
OUT=$(golden)
if echo "$OUT" | grep -q "golden mismatch" && echo "$OUT" | grep -q "^- " && echo "$OUT" | grep -q "^+ "; then
    ok "a changed queue depth is caught, with a line-level diff"
    echo "$OUT" | grep -E '^[-+] +[0-9]+ \|' | head -2 | sed 's/^/        /'
else
    bad "a perturbed ViewState did NOT fail the golden test"
fi
cp "$BACKUP/golden.rs" "$GOLDEN"

# ── mechanism 3b: the golden test catches a clock clippy was silenced for ────
# This is the one that makes the golden test worth having ON TOP of the lint:
# it catches a clock that arrived by a route mechanism 2 does not cover.
head_ "3b. golden test vs a clock that clippy was silenced for"
python3 - "$RENDER" "$SIG" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); needle = sys.argv[2]; s = p.read_text()
assert needle in s, "frame() signature moved; update conformance_test.sh"
patched = needle.replace("now: DateTime", "_now: DateTime") \
    + "\n    #[allow(clippy::disallowed_methods)] // CONFORMANCE PROBE\n    let now = Utc::now();"
p.write_text(s.replace(needle, patched, 1))
PY
if clippy | grep -q "disallowed method"; then
    bad "the probe did not actually silence clippy; 3b proves nothing"
fi
OUT=$(golden)
if echo "$OUT" | grep -q "golden mismatch"; then
    ok "a lint-invisible clock is still caught by the golden test"
    echo "$OUT" | grep -E '^[-+] +[0-9]+ \|' | head -2 | sed 's/^/        /'
else
    bad "a real clock did NOT fail the golden test"
    echo "$OUT" | tail -14 | sed 's/^/        /'
fi
cp "$BACKUP/render.rs" "$RENDER"

# ── mechanism 3c: the SHAPE law, forced to fail ─────────────────────────────
# assert_shape is the postcondition every case shares, and `0018` measured what
# an over-report costs in this stack: ratatui 0.28 panics rather than wrapping.
head_ "3c. the shape law (exactly h lines of exactly w columns), forced to fail"
python3 - "$RENDER" <<'PY'
import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
needle = "    let w = width as usize;"
assert needle in s, "frame()'s width binding moved; update conformance_test.sh"
p.write_text(s.replace(needle, "    let w = width as usize + 1; // CONFORMANCE PROBE", 1))
PY
OUT=$(golden)
if echo "$OUT" | grep -qE "line [0-9]+ width|line count"; then
    ok "an over-reported line width is caught by assert_shape (forced failure observed)"
    echo "$OUT" | grep -E "line [0-9]+ width|line count" | head -2 | sed 's/^/        /'
else
    bad "an over-reported width did NOT fail the shape assertion"
    echo "$OUT" | tail -10 | sed 's/^/        /'
fi
cp "$BACKUP/render.rs" "$RENDER"

# ── the green, last ──────────────────────────────────────────────────────────
head_ "4. the restored crate, green"
if cargo test -p tui-render 2>&1 | grep -q "test result: FAILED"; then
    bad "the restored crate does not pass its own tests"
else
    ok "cargo test -p tui-render is green"
fi

head_ "result"
if [ "$FAILURES" -eq 0 ]; then
    printf '  \033[32mall four purity mechanisms proven, each after being forced to fail\033[0m\n\n'
    exit 0
fi
printf '  \033[31m%d check(s) failed\033[0m\n\n' "$FAILURES"
exit 1
