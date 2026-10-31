#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  tui CHAT conformance test — build step 3's laws, each forced to fail.
#
#  `core:writing-tools`: a green you have not forced to fail is not evidence.
#  The tests in `render/tests/golden.rs`, `src/app.rs` and `src/store.rs` prove
#  the face behaves. THIS proves those tests can TELL. Each law below is broken
#  in the real source in the specific way it exists to catch, the matching test
#  is re-run and must go RED, and the source is restored. A mutation that leaves
#  the suite green is reported as a failure of the SUITE, which is the only
#  thing this script is for.
#
#  Why these. Every one of them fails SILENTLY when it is wrong:
#
#    * the follow-mode anchor — the view slides under him by a few rows while
#      he is reading, which looks like his own scrolling;
#    * the repaint law — stale cells that the differential renderer agrees
#      with, so no later frame ever rewrites them (`0018`);
#    * the fold's "nothing is dropped" promise — a `×n` stamp that stands for
#      events that are not actually reachable;
#    * the filter — rows missing from a transcript that does not look filtered;
#    * the block layout — a box that wraps to the wrong width, which at the
#      wrong moment is a ratatui PANIC rather than a cosmetic slip;
#    * the read-only boundary — a mutating verb reaching his live daemon.
#
#  It touches nothing outside this crate: no network, no daemon, no terminal.
#  Every source it edits is restored on every exit path including a signal; if
#  it is ever interrupted hard enough to skip the trap, `git status` says so.
#
#  Usage:  ./chat_conformance_test.sh
#  Exit:   0 every law proven discriminating; 1 otherwise.
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

cd "$REPO_ROOT" || exit 1

APP="$CRATE_DIR/src/app.rs"
STORE="$CRATE_DIR/src/store.rs"
INPUT="$CRATE_DIR/src/input.rs"
TERM="$CRATE_DIR/src/term.rs"
RENDER="$CRATE_DIR/../../libs/tui-render/src/render.rs"
MODEL="$CRATE_DIR/../../libs/tui-render/src/model.rs"
SELECT="$CRATE_DIR/../../libs/tui-render/src/select.rs"
HIT="$CRATE_DIR/../../libs/tui-render/src/hit.rs"
# **`READS`/`src/reads.rs` IS GONE (R7.S3, HOLDS R22) — see §7's retirement
# note below.** It stood here only because the (now-retired) §7 smuggled a
# second `threads.send` into it. Nothing else in this script ever touched it.
BACKUP="$(mktemp -d)"
FAILURES=0

cp "$APP" "$BACKUP/app.rs"
cp "$STORE" "$BACKUP/store.rs"
cp "$INPUT" "$BACKUP/input.rs"
cp "$TERM" "$BACKUP/term.rs"
cp "$RENDER" "$BACKUP/render.rs"
cp "$MODEL" "$BACKUP/model.rs"
cp "$SELECT" "$BACKUP/select.rs"
# `hit.rs` joined the backed-up set in `0076`, when the click door onto a
# thread got its own two anchors. A file a probe mutates and the trap does not
# restore is how a killed run leaves the tree wrong.
cp "$HIT" "$BACKUP/hit.rs"

restore() {
    cp "$BACKUP/app.rs" "$APP"
    cp "$BACKUP/store.rs" "$STORE"
    cp "$BACKUP/input.rs" "$INPUT"
    cp "$BACKUP/term.rs" "$TERM"
    cp "$BACKUP/render.rs" "$RENDER"
    cp "$BACKUP/model.rs" "$MODEL"
    cp "$BACKUP/select.rs" "$SELECT"
    cp "$BACKUP/hit.rs" "$HIT"
}
cleanup() { restore; rm -rf "$BACKUP"; }
# The scratch target tree and the source mirror go with it: bash traps are
# last-write-wins, so this line would otherwise silently discard the one
# `conformance_target.sh` set and leak both. Every other suite in this crate
# already chained it; this one did not, which is why it leaked a mirror per run.
trap 'cleanup; conformance_target_release' EXIT INT TERM

ok()    { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
bad()   { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAILURES=$((FAILURES + 1)); }
head_() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# run_one <target> <test-name>  ->  0 green | 1 red | 2 did not build
run_one() {
    local target="$1" name="$2" out
    case "$target" in
        golden) out="$(cargo test -p tui-render --test golden "$name" -- --exact 2>&1)" ;;
        # The MODULE PATH is required with --exact. A name that resolves to
        # nothing runs zero tests and still exits 0, which would silently turn
        # every proof below into a tautology — so `run_one` reports that case
        # apart from a real green (status 3) rather than accepting it.
        # `--lib`, not `--bin tui`: `0037` moved `app` (with `term`, `input`
        # and the new `event`) out of the binary and into the library, because
        # the core behaviors of the TUI being unreachable from outside
        # `main.rs` is what made "could a second face be built on this" a no.
        # The guard above is what caught the move — every proof keyed on an
        # `app::tests::` name reported "resolves to nothing" rather than a
        # false green, which is the whole reason status 3 exists.
        app)    out="$(cargo test -p tui --lib "app::tests::$name" -- --exact 2>&1)" ;;
        store)  out="$(cargo test -p tui --lib "store::tests::$name" -- --exact 2>&1)" ;;
        input)  out="$(cargo test -p tui --lib "input::tests::$name" -- --exact 2>&1)" ;;
        term)   out="$(cargo test -p tui --lib "term::tests::$name" -- --exact 2>&1)" ;;
        rlib)   out="$(cargo test -p tui-render --lib "render::tests::$name" -- --exact 2>&1)" ;;
    esac
    LAST_OUT="$out"
    if grep -q "^error\[" <<<"$out" || grep -q "^error: could not compile" <<<"$out"; then
        return 2
    fi
    if grep -qE "test result: ok\. 0 passed" <<<"$out"; then
        return 3
    fi
    grep -q "test result: ok" <<<"$out" && return 0
    return 1
}

# mutate <file> <python-edit>
mutate() { python3 - "$1" <<<"$2"; }

# prove <label> <target> <test> <file> <python-edit>
#   green now -> red after the mutation -> green again after restore.
prove() {
    local label="$1" target="$2" test="$3" file="$4" edit="$5"
    run_one "$target" "$test"
    case $? in
        0) ;;
        3) bad "$label: the test NAME resolves to nothing — this proof would be a tautology"
           return ;;
        *) bad "$label: the test is not green BEFORE the mutation — nothing can be proven"
           echo "$LAST_OUT" | tail -6 | sed 's/^/        /'
           return ;;
    esac
    if ! mutate "$file" "$edit"; then
        bad "$label: the mutation did not apply — the source moved under this script"
        restore
        return
    fi
    run_one "$target" "$test"
    case $? in
        1) ok "$label"
           grep -E "panicked at|assertion|golden mismatch" <<<"$LAST_OUT" | head -1 | sed 's/^/        /' ;;
        0) bad "$label: the mutation left the test GREEN — the test cannot tell" ;;
        2) bad "$label: the mutated crate did not build; the mutation is wrong, not the code"
           grep "^error" <<<"$LAST_OUT" | head -3 | sed 's/^/        /' ;;
        3) bad "$label: the mutated run executed no tests" ;;
    esac
    restore
}

# ── THE GUARD THIS SCRIPT DID NOT HAVE, AND PAID FOR ────────────────────────
#
# Every mutation below is restored by `restore`, which runs from a trap on EXIT,
# INT and TERM. A SIGKILL skips all three — and a run killed by an outer
# timeout while a mutation was applied left `store.rs` and `model.rs` MUTATED,
# green on their own tests and red on four unrelated ones, which read as four
# real regressions. It cost half an hour to recognise.
#
# The probe marker is the tell, so the cheap fix is to look for it BEFORE doing
# anything: a corrupted tree now refuses to run and says which file, instead of
# producing a plausible wrong answer.
STRAY="$(cd "$CRATE_DIR" && grep -rln 'CONFORMANCE PROBE' src ../../libs/tui-render/src 2>/dev/null)"
if [ -n "$STRAY" ]; then
    printf '\033[31mREFUSING TO RUN\033[0m — a previous run was killed mid-mutation and left probes in:\n'
    printf '  %s\n' $STRAY
    printf 'Restore them (git diff will show each one) before running this again.\n'
    exit 1
fi

printf '\033[1mtui — build step 3 (the read-only chat), every law forced to fail\033[0m\n'

# ─────────────────────────────────────────────────────────────────────────────
head_ "1. FOLLOW MODE — an arrival must not move what is on screen"
# ─────────────────────────────────────────────────────────────────────────────

# (a) The RENDER half. Lay the new blocks out with the rest and the window,
#     which is anchored from the bottom of the content, slides down by however
#     many rows they took. This is the exact bug the golden test caught during
#     the build, and it is the one that reads as his own scrolling.
prove "a viewport anchored to growing content is caught yanking" \
    golden an_arrival_while_scrolled_up_does_not_move_what_is_on_screen "$RENDER" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''    let held = if vs.viewport.follow {
        &vs.blocks[..]
    } else {
        &vs.blocks[..vs.blocks.len().saturating_sub(vs.viewport.new_since)]
    };'''
assert old in s, 'the follow anchor moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '    let held = &vs.blocks[..]; // CONFORMANCE PROBE', 1))"

# (b) The CHIP. Without it an arrival is silent: the view correctly does not
#     move, and nothing anywhere says the bottom grew.
prove "a missing new-message chip is caught" \
    golden the_new_chip_is_pinned_to_the_bottom_edge_while_follow_is_off "$RENDER" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '.then(|| format!(\"  ↓ {} new — click here, or end\", vs.viewport.new_since));'
assert old in s, 'the chip moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '.then(String::new); // CONFORMANCE PROBE: the chip goes blank', 1))"

# (c) The SHELL half. The freeze point is what makes the anchor mean anything;
#     without it `new_since` is always 0 and (a) degrades to the yank.
prove "a freeze point that never fires is caught" \
    app an_event_on_another_thread_does_not_chip_the_open_one "$APP" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''    fn freeze(&mut self) {
        let n = self.vs.blocks.len();'''
assert old in s, 'freeze() moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '''    fn freeze(&mut self) {
        if true { return; } // CONFORMANCE PROBE
        let n = self.vs.blocks.len();''', 1))"

# **RE-KEYED `0105`.** The anchor below read `Some(n) => ...saturating_sub(n)`.
#     `0097` turned the freeze point from a bare `Option<usize>` into
#     `Option<Frozen>` — it now carries the pinned tail BLOCK as well as the
#     count — so the binding became `Some(f)` and `n` became `f.count`. The
#     worker that made that change was KILLED before it re-keyed here, and this
#     suite exited 1 on the stale anchor. The guard did exactly its job: it said
#     the source had moved under the script rather than blaming the test.
# (d) The chip must count ROWS, not events. One transport event is not one
#     block — the lane folds runs — so a per-event counter promises rows that
#     are not going to be there.
prove "a chip counting events rather than rows is caught" \
    app the_chip_counts_rows_he_will_actually_see_not_events_that_arrived "$APP" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '            Some(f) => self.vs.blocks.len().saturating_sub(f.count),'
assert old in s, 'arrivals() moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '            Some(_) => 6, // CONFORMANCE PROBE: an event counter would say 6', 1))"

# ─────────────────────────────────────────────────────────────────────────────
head_ "2. THE REPAINT LAW — every moment the screen can move without us"
# ─────────────────────────────────────────────────────────────────────────────

# THE CLIPBOARD WRITE. It puts real escape bytes at the terminal from inside
# raw mode, around ratatui — so anything the terminal does not consume lands on
# the screen as text the differential renderer then agrees with forever. It
# REPLACES the mouse-handoff set-point, which went with `M` (`0041`).
prove "a clipboard write that does not force a repaint is caught" \
    app a_clipboard_write_forces_a_repaint "$APP" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '        self.force_repaint(\"a clipboard sequence went straight at the terminal\");'
assert old in s, 'the clipboard set-point moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '        // CONFORMANCE PROBE: set-point removed', 1))"

# The link changing hands. A reconnect paints over a screen the last session
# left behind, and the diff agrees with every stale cell on it.
prove "a reconnect that does not force a repaint is caught" \
    app a_disconnect_forces_a_full_repaint_and_keeps_the_rows "$APP" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '            self.force_repaint(\"the link changed hands\");'
assert old in s, 'the relink set-point moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '            // CONFORMANCE PROBE: set-point removed', 1))"

# The set-points must stay ENUMERABLE IN ONE GREP (`0023` CLASS 2). This is a
# property of the source, so it is checked as one — and forced, below.
head_ "2b. the set-points are enumerable in one grep"

# Three source-text properties, three functions OVER A FILE, and three mutated
# COPIES to force each one. Copies rather than the tree because this section
# does not compile anything: the property is about the TEXT, so the mutant is
# text, and nothing can be left dirty.
call_sites()       { grep -n 'force_repaint(' "$1" | grep -v 'fn force_repaint' | grep -v '///' | grep -c 'force_repaint('; }
distinct_reasons() { grep -o 'force_repaint("[^"]*")' "$1" | sort -u | grep -c 'force_repaint('; }
raw_assignments()  { grep -cE 'needs_full_repaint = true' "$1"; }

# **THE MEASUREMENT WAS WRONG BEFORE THE BAR WAS** (`0065`). `SETPOINTS` was a
# bare `grep -c 'force_repaint('`, which counts the FUNCTION'S OWN DEFINITION
# and a doc-comment line along with the call sites — it read 8 where there are
# 6 set-points, while the failure message called the number "call sites". A bar
# over the wrong quantity cannot be made to mean anything by moving the bar, so
# the quantity is fixed first and the floor is set on the corrected number.
#
# And the distinct-reason count was `echo "$GREPPED" | wc -l`, which returns 1
# for an EMPTY result — so a source with no set-points left in it at all scored
# one, not zero. Counted with `grep -c` it scores zero, which is what it is.
#
# **THE RULING ON THE BARS.** Both are raised from `>= 5` / `>= 6` to a FLOOR AT
# THE ACTUAL, and deliberately NOT to an equality. The failure these bars are
# the last line against is a set-point going MISSING — only two of the six are
# proven individually by name (the clipboard write and the relink, both just
# above); resize, `^L`, startup and the keyboard-query answer have no per-name
# proof anywhere in this crate. Adding a set-point, by contrast, is normal and
# wanted work, and an equality would go red on it and teach the next reader to
# edit the bar to get their green, which is how a check becomes a rubber stamp.
# The two anchors in this crate that DO assert an exact count are exact at ONE,
# where a second occurrence is itself the defect; that is an invariant, and an
# equality here would only be a record of today's number. Raise these floors
# when you add a set-point; never lower one to make a red go away.
REASON_FLOOR=6
CALLSITE_FLOOR=6
SETPOINTS=$(call_sites "$APP")
REASONS=$(distinct_reasons "$APP")
grep -o 'force_repaint("[^"]*")' "$APP" | sort -u | sed 's/^/        /'
if [ "$REASONS" -ge "$REASON_FLOOR" ] && [ "$SETPOINTS" -ge "$CALLSITE_FLOOR" ]; then
    ok "every set-point is one call, and one grep finds them all ($SETPOINTS call sites, $REASONS distinct reasons)"
else
    bad "the set-point enumeration is incomplete: $SETPOINTS call sites (floor $CALLSITE_FLOOR), $REASONS distinct reasons (floor $REASON_FLOOR) — a set-point was DELETED"
fi

# FORCED (i): delete one set-point. This is the mutation that shows what the
# raised floor bought — under the old `>= 6` on the old, inflated count, this
# copy was GREEN.
MUT="$BACKUP/app_setpoint_deleted.rs"
sed 's/Input::Resize => self.force_repaint("resize"),/\/\/ CONFORMANCE PROBE: set-point deleted/' "$APP" > "$MUT"
if [ "$(call_sites "$MUT")" -eq "$SETPOINTS" ]; then
    bad "2b(i): the deletion mutant is identical to the source — the anchor moved and this forced failure proves nothing"
elif [ "$(call_sites "$MUT")" -ge "$CALLSITE_FLOOR" ]; then
    bad "2b(i): a DELETED set-point is still above the floor — the call-site bar proves nothing"
else
    ok "2b(i) FORCED: one deleted set-point IS caught ($SETPOINTS -> $(call_sites "$MUT"), floor $CALLSITE_FLOOR)"
fi

# FORCED (ii): keep every call site but give two of them the SAME reason. The
# call-site count does not move at all, so this is the mutation that says the
# distinct-reason bar is a second property and not a restatement of the first —
# a set-point whose reason is a copy of another's is one nobody can find in a
# log, which is what `0023` asked the enumeration for in the first place.
MUT2="$BACKUP/app_reason_collapsed.rs"
sed 's/force_repaint("\^L")/force_repaint("resize")/' "$APP" > "$MUT2"
if [ "$(distinct_reasons "$MUT2")" -eq "$REASONS" ]; then
    bad "2b(ii): the collapse mutant changed nothing — the anchor moved and this forced failure proves nothing"
elif [ "$(call_sites "$MUT2")" -ne "$SETPOINTS" ]; then
    bad "2b(ii): the collapse mutant also changed the CALL SITE count — it does not isolate the reason bar"
elif [ "$(distinct_reasons "$MUT2")" -ge "$REASON_FLOOR" ]; then
    bad "2b(ii): two set-points sharing one reason is still above the floor — the reason bar proves nothing"
else
    ok "2b(ii) FORCED: two set-points collapsed onto one reason IS caught ($REASONS -> $(distinct_reasons "$MUT2") reasons, call sites unchanged at $(call_sites "$MUT2"))"
fi

# Forced: a set-point written as a raw field assignment is INVISIBLE to that
# grep, which is the whole reason the rule is "one function, never the flag".
# EXACTLY ONE raw assignment is legal — the one inside `force_repaint` itself.
# A second is a set-point the enumeration above cannot see.
RAW=$(raw_assignments "$APP")
if [ "$RAW" -eq 1 ]; then
    ok "the flag is set in exactly one place (a second WOULD be seen; count=$RAW)"
else
    bad "the flag is set in $RAW places — a set-point bypasses force_repaint()"
fi
# FORCED (iii): and here is that second one, which no count of call sites could
# ever see.
MUT3="$BACKUP/app_second_assignment.rs"
{ cat "$APP"; printf '        self.needs_full_repaint = true; // CONFORMANCE PROBE\n'; } > "$MUT3"
if [ "$(raw_assignments "$MUT3")" -eq 1 ]; then
    bad "2b(iii): a second raw assignment was NOT seen — the exact-one bar proves nothing"
else
    ok "2b(iii) FORCED: a second raw needs_full_repaint assignment IS caught ($(raw_assignments "$MUT3") found, exactly 1 is legal)"
fi

# ─────────────────────────────────────────────────────────────────────────────
head_ "3. THE FOLD — a ×n stamp that stands for events nobody can reach"
# ─────────────────────────────────────────────────────────────────────────────

prove "task_progress that does not fold to one live row is caught" \
    store task_progress_folds_to_one_live_row_and_expands_to_every_one_of_them "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '        ThreadEvent::TaskProgress { .. } => Fold::PerTask,'
assert old in s, 'fold_of moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '        ThreadEvent::TaskProgress { .. } if false => Fold::PerTask, // CONFORMANCE PROBE', 1))"

# THE PROMISE, not the display. `x` must reach every event a stamp stands for;
# a fold that survives expansion has DROPPED them and only looks like it has not.
prove "a fold that survives expansion (so nothing is reachable) is caught" \
    store task_progress_folds_to_one_live_row_and_expands_to_every_one_of_them "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '        let fold = if open { Fold::Never } else { fold_of(&r.event) };'
assert old in s, 'the expansion switch moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '        let fold = fold_of(&r.event); let _ = open; // CONFORMANCE PROBE', 1))"

prove "a run fold that swallows the kind after it is caught" \
    store a_run_is_keyed_on_the_fact_and_not_on_the_wire_kind "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '                    Some((k, idx)) if *k == fk => {'
assert old in s, 'the run guard moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '                    Some((_k, idx)) => { // CONFORMANCE PROBE: any fact joins the run', 1))"

# ─────────────────────────────────────────────────────────────────────────────
head_ "4. THE STDOUT-ONLY TOGGLE — what DIR-4's discharge left, and its announcement"
# ─────────────────────────────────────────────────────────────────────────────
#
# **THIS SECTION USED TO BE "THE FILTER".** It had four proofs keyed on
# `filter.allows(lane)`, `Filter::narrowed()`, the filter overlay's golden and
# `lane_of`'s totality over forty-four kinds. Round six deleted all four
# anchors: *"filter is too much. Just drop that feature. We'll just tune the
# interface. I had that request because I wanted a simple toggle."*
#
# The proofs are RE-KEYED rather than dropped, because the PROPERTIES survived
# the machinery: rows are held back, the header says so, the header says so at
# forty columns, and the predicate that decides is narrow. One boolean now does
# what nine bits did.

prove "a toggle that hides nothing is caught" \
    store the_stdout_toggle_keeps_the_boxes_and_drops_the_rest "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '        if stdout_only && !is_stdout(&r.event) {'
assert s.count(old) == 1, 'ANCHOR-MISSING stdout-gate — store::project holds the toggle gate once and only once'
p.write_text(s.replace(old, '        if false { // CONFORMANCE PROBE: the toggle holds nothing back', 1))"

# DIR-6's "at all times" includes at 40 columns, and so does the collapse's
# announcement. The header drops by RANK, not right-to-left; ranking the notice
# below the badge's own scope label is the specific slip the live run found.
prove "a header that ranks the stdout notice below chrome is caught" \
    golden the_queue_badge_survives_the_crop_at_every_width "$RENDER" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = 'parts.push((2, vec![dot(), Seg::new(\"stdout only\", palette::amber())]));'
assert s.count(old) == 1, 'ANCHOR-MISSING stdout-chip-rank — the notice rank moved or is written twice'
p.write_text(s.replace(old, 'parts.push((9, vec![dot(), Seg::new(\"stdout only\", palette::amber())])); // CONFORMANCE PROBE: first thing dropped', 1))"

# A collapsed view that does not LOOK collapsed is a transcript with a hole in
# it. **AND THE WITNESS IS WHY THIS PROOF IS NOT AN ABSENCE**: the golden it is
# keyed on asserts the frame is positively RIGHT — a header row, a footer row
# and three rows of body — before it asserts anything about the chip, so a face
# that died on startup cannot score it.
prove "a collapsed view that does not announce itself is caught" \
    golden no_frame_in_any_mode_says_filter_and_the_face_still_draws "$RENDER" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''    if vs.stdout_only {
        parts.push((2, vec![dot(), Seg::new(\"stdout only\", palette::amber())]));
    }'''
assert s.count(old) == 1, 'ANCHOR-MISSING stdout-chip — the notice moved or is written twice'
p.write_text(s.replace(old, '    // CONFORMANCE PROBE: the notice goes silent', 1))"

# **THE PREDICATE IS NARROW AND IT IS THE ONLY CLASSIFIER LEFT.** The old proof
# here asserted `lane_of` total over every daemon kind so none fell through to
# `Lane::Other`. There is no table to be total any more; there is one question,
# and the thing that can go wrong is that it answers YES too often.
prove "a kind that counts as stdout when it is not is caught" \
    store only_what_was_said_is_stdout_and_an_unknown_kind_is_not "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '            \"user_message\" | \"assistant_text\" | \"context_injected\"'
assert s.count(old) == 1, 'ANCHOR-MISSING stdout-kinds — is_stdout moved or is written twice'
p.write_text(s.replace(old, '            \"user_message\" | \"assistant_text\" // CONFORMANCE PROBE: injected content stops counting', 1))"

# ─────────────────────────────────────────────────────────────────────────────
head_ "4b. CLICK NEVER DRILLS — the round-six reversal, forced BOTH ways"
# ─────────────────────────────────────────────────────────────────────────────
#
# *"As I requested once before, clicking on a thread shouldn't take me into it.
# Only right can do it."* Two proofs, deliberately opposed, because a round that
# deleted the drill and quietly broke `→` would satisfy either one alone. Both
# are keyed on the SAME test, which is the one round four left asserting the
# wrong thing and this round turned around.

prove "a click that takes him into a thread is caught" \
    app a_click_on_a_threads_name_selects_it_and_right_is_what_opens_it "$HIT" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = 'Via::Click) => {\n            Some(Intent::Select(i))'
assert s.count(old) == 1, 'ANCHOR-MISSING click-title — the ThreadTitle click arm moved or is written twice'
p.write_text(s.replace(old, 'Via::Click) => {\n            Some(Intent::Open) // CONFORMANCE PROBE: the click drills again', 1))"

prove "a right-arrow that stops opening a thread is caught" \
    app a_click_on_a_threads_name_selects_it_and_right_is_what_opens_it "$HIT" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '| (Target::ThreadRow(_), Mode::List, Via::Key) => Some(Intent::Open),'
assert s.count(old) == 1, 'ANCHOR-MISSING key-open — the arrow-open arm moved or is written twice'
p.write_text(s.replace(old, '| (Target::ThreadRow(_), Mode::List, Via::Key) => None, // CONFORMANCE PROBE: the arrow stops opening', 1))"

# ─────────────────────────────────────────────────────────────────────────────
head_ "5. THE LAYOUT — every line exactly the width it was handed"
# ─────────────────────────────────────────────────────────────────────────────

# `assert_block_height` is the assertion `assert_shape` CANNOT make: a renderer
# that laid every block out on one line would satisfy the shape law perfectly.
#
# **ANCHOR RESTATED, 0129.** It used to read
# `box_width(w).saturating_sub(2).max(1)`, which was `msg_inner`'s whole body
# while both lanes wrapped at one measure. His margins are the specification
# now — *"The agent margin should be like 10 on the right"* — so the assistant
# spends no columns on a box it does not have and its measure IS the block.
# **THE GUARD WAS RIGHT AND THE CODE WAS THE CHANGE**, the same call `0064`
# made below, so the anchor is restated to the line as it now reads rather than
# loosened into a pattern that would match anything. The property is unchanged:
# halve the measure and the block-height golden goes red.
prove "a message wrapped to the wrong width is caught by the block-height golden" \
    golden a_message_wraps_to_the_rows_its_box_width_implies "$RENDER" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '\n    box_width(w)\n'
assert s.count(old) == 1, 'ANCHOR-MISSING message-measure — render::msg_inner moved or is written more than once'
p.write_text(s.replace(old, '\n    (box_width(w) / 2).max(1) // CONFORMANCE PROBE\n', 1))"

# **RE-KEYED, 0117.** The third lane became the CENTRED INFO BLOB — *"info
# blobs should be centered with a max character size that enforces a margin on
# each side… It can expand or collapse vertically."* — so a blob now has two
# states and only the CLOSED one is one row. The property the guard is
# defending is unchanged and is if anything sharper: a closed blob that started
# wrapping would take the vertical growth that opening is supposed to be the
# only cause of.
prove "a closed info blob that stops being one row is caught" \
    golden a_closed_info_blob_is_always_exactly_one_row "$RENDER" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '    if !open {'
assert s.count(old) == 1, 'ANCHOR-MISSING closed-blob-arm — render::blob\'s closed arm moved or is written more than once'
p.write_text(s.replace(old, '    if false { // CONFORMANCE PROBE: every blob wraps', 1))"

# The narrow list: the cwd is the only thing he can recognise a thread by.
#
# **ANCHOR RESTATED, 0064.** The drop rule used to read
# `while tail.len() > 1 && w.saturating_sub(tail_w + 1) < LIST_LEFT_FLOOR {`.
# `0064` gave the tail per-segment DROP RANKS (so display order and collapse
# order stop being forced to agree) and made the floor a function of the width
# (`render::content_floor`), because a fixed 23 columns is generous at 150 and
# derisory at 60 — which is how a band row came to spend 33 of 60 columns on
# metadata and 24 on the summary. The GUARD WAS RIGHT AND THE CODE WAS THE
# CHANGE, so the anchor is restated to the line as it now reads rather than
# loosened into a pattern that would match anything. The property is unchanged:
# disarm the rule and the metadata never gives way, so the cwd pays instead.
prove "a narrow list that drops the cwd before the metadata is caught" \
    golden a_narrow_list_drops_metadata_before_it_drops_the_cwd "$RENDER" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '    while w.saturating_sub(tail_w + 1) < content_floor(w) {'
assert s.count(old) == 1, 'ANCHOR-MISSING narrow-list-drop-rule — the drop rule moved or is written more than once (see render::drop_tail_to_floor)'
p.write_text(s.replace(old, '    while false { // CONFORMANCE PROBE: metadata never gives way', 1))"

# ─────────────────────────────────────────────────────────────────────────────
head_ "5b. THE DRAG — IS-21, and every way it can go quietly wrong"
# ─────────────────────────────────────────────────────────────────────────────
# He has raised this defect twice. Every one of the five things below fails
# SILENTLY: a drag that also activates looks like a misfire he caused; a copy
# that takes the wrong characters looks like a copy; a wrapper that stops being
# a wrapper reaches tmux and is dropped with no error anywhere; and an
# autoscroll that does not scroll looks like a selection that simply stopped.

# (a) CLICK AND DRAG ARE MUTUALLY EXCLUSIVE — the operator's literal words,
#     "It needs to be treated separately from a click". The obvious regression
#     is the one this mutation writes: treat every release as a click.
prove "a release that always activates (so a drag activates too) is caught" \
    input a_drag_never_clicks_and_a_click_never_drags "$INPUT" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = 'if std::mem::take(&mut self.moved) {'
assert old in s, 'the classifier moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, 'if false { let _ = std::mem::take(&mut self.moved); // CONFORMANCE PROBE', 1))"

# (b) THE OTHER DIRECTION: acting on the PRESS. A click and the start of a drag
#     are byte-identical at press time, so anything that acts there has acted
#     before it knows which gesture it is in.
prove "an activation on the press is caught" \
    input a_press_anchors_and_never_activates "$INPUT" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '                Some(Input::Press { col, row })'
assert old in s, 'the press arm moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '                Some(Input::Click { col, row }) // CONFORMANCE PROBE', 1))"

# (c) THE LOGICAL-TEXT MAPPING. `col0` says which screen column a row's first
#     copyable character sits at. Off by the gutter and the rail, and every
#     offset on the row is wrong by four characters — the highlight lands
#     beside the text and the clipboard gets something he did not point at,
#     with nothing on screen looking wrong. The most silent failure in the
#     whole path, which is why it has a test of its own to break.
# **RE-KEYED, 0117.** The rail is gone — *"You don't need to have the quote
# line on the left of every message. It's distracting."* — so the assistant's
# text begins two columns further left and the test that says so was renamed
# with it. The probe is the same one: offset the map from the row it describes.
prove "a row whose text map is offset from the row it describes is caught" \
    rlib a_message_rows_text_starts_after_the_gutter "$RENDER" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = 'Sender::Assistant => (GUTTER as u16, msg_inner(w)),'
assert s.count(old) == 1, 'ANCHOR-MISSING message-row-col0 — the assistant arm of lay_out\'s (col0, measure) moved or is written more than once'
p.write_text(s.replace(old, 'Sender::Assistant => (0, msg_inner(w)), // CONFORMANCE PROBE', 1))"

# (d) THE TMUX WRAPPER. `input_osc_52()` returns early unless set-clipboard is
#     `on`, and he runs `external` — so the DCS passthrough is the only route
#     that reaches his terminal, and it only works if every ESC of the inner
#     sequence is DOUBLED. Stop doubling and tmux eats the sequence: no error,
#     no reply, nothing on the clipboard. Which is the defect being fixed.
prove "a passthrough wrapper that stops doubling its escapes is caught" \
    term inside_tmux_the_sequence_is_wrapped_and_every_escape_is_doubled "$TERM" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
esc = chr(92) + 'x1b'
q = chr(34)
old = \"osc.replace('\" + esc + \"', \" + q + esc + esc + q + \")\"
assert s.count(old) == 2, 'the doubling moved (found ' + str(s.count(old)) + '); update chat_conformance_test.sh'
p.write_text(s.replace(old, 'osc.clone()'))"

# (e) AUTOSCROLL. A pointer held still at the edge of the band produces NO
#     terminal reports, so without the tick a selection simply stops at the
#     first visible row — which looks like the drag ending, not like a missing
#     feature.
prove "an autoscroll tick that does not scroll is caught" \
    app a_drag_held_at_the_top_edge_scrolls_and_the_anchor_does_not_move "$APP" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '    fn scroll_band(&mut self, up: bool) {'
assert old in s, 'scroll_band moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, old + chr(10) + '        if true { let _ = up; return; } // CONFORMANCE PROBE', 1))"

# ─────────────────────────────────────────────────────────────────────────────
head_ "6. HONESTY — unknown never renders as zero, a caused cancel is not a warning"
# ─────────────────────────────────────────────────────────────────────────────

prove "an unknown rendered as a zero is caught" \
    golden unknowns_render_as_question_marks_and_never_as_zeroes "$MODEL" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '            Known::Unknown => \"?\".into(),'
assert old in s, 'Known::label moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '            Known::Unknown => \"0\".into(), // CONFORMANCE PROBE', 1))"

prove "a caused cancel rendered as a warning is caught" \
    store an_operator_interrupt_is_calm_and_an_unexplained_one_is_not "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '            if trigger == \"operator\" || trigger == \"user\" {'
assert old in s, 'the interrupt severity moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '            if false { // CONFORMANCE PROBE: every cancel is a warning', 1))"

prove "a status push that erases last_seq is caught" \
    store a_status_push_updates_what_it_knows_and_erases_nothing_else "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''                f.queued = Known::Known(queued as u32);
                f.unread = Known::Known(unread as u32);'''
assert old in s, 'the status push arm moved; update chat_conformance_test.sh'
p.write_text(s.replace(old, '''                f.queued = Known::Known(queued as u32);
                f.unread = Known::Known(unread as u32);
                f.last_seq = None; // CONFORMANCE PROBE''', 1))"

# The paging decision. `1000+` was retired in `0031` because a LIMIT-ed page
# cannot say how many threads exist; the face now counts an INDEX instead,
# which is a whole number of a real set. The probe puts the `+` back.
#
# **ANCHOR RE-KEYED, 0076, AND IT WAS FOUND BROKEN ON ARRIVAL.** It anchored
# `let t = store.owned_count();` in the ROOT header. `0072` cut the thread total
# out of that header on his own instruction ("I don't need to see … 305
# threads"), the line went with it, and this check has been reporting
# `the mutation did not apply — the source moved under this script` ever since:
# **the wrong diagnosis wearing the shape of a real failure.** That is the
# script's own §0 lesson, and it happened to this script.
#
# The PROPERTY is unchanged and is not about the root: *no number on this face
# may be a `LIMIT`-ed page's length wearing a `+`.* The test itself moved with
# the header — it now asserts the LIST header's count positively
# (`harness · 2 threads`) — so the anchor follows it there. That count is read
# off the project's INDEX, which is exactly the kind of number a `+` would be a
# lie about.
prove "a windowed page rendered as a fleet size is caught" \
    store the_root_counts_a_real_set_and_never_renders_a_windowed_plus "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''                Some(p) => format!(
                    \"{} · {} thread{} · {} active\",
                    p.name,'''
assert s.count(old) == 1, 'ANCHOR-MISSING list-header-count — the project list header moved or is written more than once (see store::header)'
p.write_text(s.replace(old, '''                Some(p) => format!(
                    \"{} · {}+ thread{} · {} active\", // CONFORMANCE PROBE
                    p.name,''', 1))"

# ─────────────────────────────────────────────────────────────────────────────
head_ "6b. WHERE BACK GOES, WHAT A CLICK DOES, AND THE HASH THAT IS NOWHERE"
# ─────────────────────────────────────────────────────────────────────────────
# `0076`, and all three are HIS OWN WORDS rather than the face's design:
#
#   > when I go into a thread and then hit escape, it brings me back to the
#   > list of all raw threads. And I never want to see that.
#
#   > currently, clicking on a thread opens its options, not the thread. It
#   > should open the thread.
#
#   > don't show me thread or task hash names. not useful to me.
#
# The third one had a guard that NAMED it and was green over four live
# violations for two rounds (`0073`), which is why the sweep below is a machine
# over every MODE rather than a list of projections.

prove "esc out of a band-opened thread landing in a project he never entered is caught" \
    app a_thread_opened_from_a_root_band_escapes_back_to_the_root "$APP" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''                    if self.ascend() == Mode::Projects {
                        self.store.leave_project();
                    }'''
assert s.count(old) == 1, 'ANCHOR-MISSING esc-pops-the-origin — the Mode::Thread arm of Intent::Back moved (see App::ascend)'
p.write_text(s.replace(old, '                    self.vs.mode = Mode::List; // CONFORMANCE PROBE', 1))"

prove "a drill that enters a project behind his back is caught" \
    app a_thread_opened_from_a_root_band_escapes_back_to_the_root "$APP" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''    fn drill_to_thread(&mut self, id: String) {
        self.origin.push(self.view());'''
assert s.count(old) == 1, 'ANCHOR-MISSING drill-records-its-origin — App::drill_to_thread moved'
p.write_text(s.replace(old, '''    fn drill_to_thread(&mut self, id: String) {
        if let Some(i) = self.store.projects().iter().position(|p| p.threads.iter().any(|t| t.id == id)) { self.store.enter_project(i); } // CONFORMANCE PROBE
        self.origin.push(self.view());''', 1))"

prove "a drill that forgets where he came from is caught" \
    app a_thread_drilled_through_a_project_escapes_back_to_that_project "$APP" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '        self.origin.push(self.view());'
assert s.count(old) == 1, 'ANCHOR-MISSING drill-push — the one origin push moved or is written more than once'
p.write_text(s.replace(old, '        // CONFORMANCE PROBE: the origin is not recorded', 1))"

# **THE CLICK-DRILL PROOF THAT USED TO SIT HERE IS IN SECTION 4b NOW**, pointing
# the other way: it proved the click OPENED the thread, and round six reversed
# that. See "a click that takes him into a thread is caught".

# **THIS ANCHOR WAS DEAD WHEN `0134` INHERITED THE TREE, AND IT IS RE-KEYED
# RATHER THAN DELETED.** It read `'            Some(Intent::OpenSheet(i))'` —
# the braced arm shape `hit.rs` used at `c0a34cc`. `0135` added `Mode::SeeAll(_)`
# to the same arm at merge time, rustfmt reflowed it to the parenthesised
# `) => Some(...)` form, and the assertion below stopped matching. **A probe
# whose mutation cannot apply reports `the source moved under this script`,
# which is the right diagnosis and the wrong colour: nothing was being proved.**
# Re-keyed to the shape the arm actually has. If it moves again, MOVE THIS
# STRING — do not weaken the `== 1`, which is the half that makes the mutation
# unambiguous.
prove "cutting the key door onto the sheet (so \`o\` goes dead) is caught" \
    app a_click_on_a_threads_name_selects_it_and_right_is_what_opens_it "$HIT" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = ') => Some(Intent::OpenSheet(i)),'
assert s.count(old) == 1, 'ANCHOR-MISSING o-opens-the-options — the ThreadTitle Via::Key arm moved (see hit::activation)'
p.write_text(s.replace(old, ') => None, // CONFORMANCE PROBE', 1))"

# THE SWEEP MACHINE, forced on a builder the OLD sweep never covered. The point
# is not that a hash is caught — it is WHERE: the thread header was green under
# `no_row_prints_a_hash_it_was_handed` while it drew `5f4413b1` on his screen.
prove "a hash back on the THREAD HEADER is caught (the old sweep stays green)" \
    app no_surface_of_this_face_prints_a_hash_on_the_glass_or_the_clipboard "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''            match store.label_of(id) {
                name if name.is_empty() => state_word(store, id),
                name => format!(\"{} · {}\", name, state_word(store, id)),
            },'''
assert s.count(old) == 1, 'ANCHOR-MISSING thread-header-names-its-thread — store::header Thread arm moved'
p.write_text(s.replace(old, '            format!(\"{} · {}\", &id[..8.min(id.len())], state_word(store, id)), // CONFORMANCE PROBE', 1))"

# And the CLIPBOARD half, which no frame can see: a copied run may be richer
# than the row it is dragged over, which is all of `DEC-27`.
prove "a hash back on the CLIPBOARD ONLY (never drawn) is caught" \
    app no_surface_of_this_face_prints_a_hash_on_the_glass_or_the_clipboard "$SELECT" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''pub fn thread_run(r: &crate::model::ThreadRow) -> String {
    thread_label(r).0
}'''
assert s.count(old) == 1, 'ANCHOR-MISSING thread-run-is-the-label — select::thread_run moved'
p.write_text(s.replace(old, '''pub fn thread_run(r: &crate::model::ThreadRow) -> String {
    format!(\"{} {}\", r.id.chars().take(8).collect::<String>(), thread_label(r).0) // CONFORMANCE PROBE
}''', 1))"

# **AND THE WITNESS IS FORCED TOO.** A face that names no thread contains no
# hash either, so an absence-only sweep would score a triumphant pass on a dead
# screen. This mutation leaves every hash absent and must STILL go red.
prove "a face that names no thread at all is caught (the absence alone proves nothing)" \
    app no_surface_of_this_face_prints_a_hash_on_the_glass_or_the_clipboard "$STORE" \
"import sys, pathlib
p = pathlib.Path(sys.argv[1]); s = p.read_text()
old = '''    pub fn label_of(&self, id: &str) -> String {
        if id.is_empty() {
            return String::new();
        }'''
assert s.count(old) == 1, 'ANCHOR-MISSING label-of — store::label_of moved'
p.write_text(s.replace(old, '''    pub fn label_of(&self, id: &str) -> String {
        if !id.is_empty() {
            return String::new(); // CONFORMANCE PROBE
        }''', 1))"

# ─────────────────────────────────────────────────────────────────────────────
#  **SECTION 7, "THE MUTATION SURFACE", IS RETIRED (R7.S3, HOLDS R22).**
#
#  It asserted the face's whole set of daemon-mutating methods by grepping
#  `src/methods.rs` and backing up/mutating `src/reads.rs` — both deleted.
#  `src/sends.rs`'s own module doc says why: "`methods.rs` is now DELETED,
#  because a list of names a face can speak is a weaker thing than a TYPE:
#  [`crate::acts::Verb`] makes it exhaustive. A verb this face has no variant
#  for cannot be constructed, so there is nothing left for a grep to miss."
#  `src/reads.rs`'s own READ-ONLY claim moved the same way: "four of its six
#  reads moved to the mirror and two had no shape to move to, and those two
#  live in [`crate::queue_reads`]."
#
#  Measured in this worktree before retiring it: every one of section 7's
#  backup/restore `cp`s against `src/reads.rs` failed
#  (`No such file or directory`) on every single `restore()` call for the
#  whole script, not only inside §7 — because `restore()` ran unconditionally
#  after every `prove()`, from section 1 onward. That was pure stderr noise
#  (the file was never there to lose), but once inside §7 itself the backup
#  failure left `$BACKUP/reads.rs` absent, so §7's own grep/mutate steps
#  against a nonexistent file either no-opped or raised a Python
#  `FileNotFoundError` depending on which check ran — neither is a FAIL this
#  suite's own vocabulary can name, and neither is evidence about anything
#  this face ships.
#
#  **The property section 7 existed for is now a TYPE-level guarantee,
#  checked where the type lives:** `acts.rs` carries
#  `tests::the_model_act_goes_to_the_aui_and_the_daemons_verb_is_never_spoken`
#  and `tests::this_module_never_names_the_daemons_trap_verb`; `sends.rs`
#  carries its own test that it issues exactly the one mutation its header
#  claims. A conformance suite re-deriving that guarantee by grepping deleted
#  files would be a second, weaker copy of a check `cargo test -p tui`
#  already runs — the same shape `src/methods.rs`'s own retirement argues
#  against.
#
# ─────────────────────────────────────────────────────────────────────────────
head_ "8. the restored crate, green"
# ─────────────────────────────────────────────────────────────────────────────
# **BOTH GATES TAKE THEIR VERDICT FROM CARGO'S EXIT STATUS, and that is a
# REPAIR, not a tidy-up (`0076`).**
#
# They were `cargo … | grep -q <needle>`, and a grep over cargo's output is not
# a verdict, because cargo is allowed to say NOTHING. MEASURED: this script
# reported `clippy -D warnings is clean on both crates` on THREE consecutive
# full runs of a tree where the identical command exits 101 with five real
# lints — `field_reassign_with_default` ×4 and `len_zero`, live in the shipped
# tree since `0072`, whose own report claims this crate clean at exactly this
# gate. A cached target dir replays a success and prints no diagnostics, the
# needle is absent, and absence was being read as health.
#
# That is this round's own recurring finding arriving at the gate that was
# supposed to catch it: **an absence is never evidence.** The exit status is
# the thing cargo cannot decline to produce.
#
# The `cargo test` gate had the SAME hole in a nastier shape: a crate that does
# not COMPILE prints no `test result: FAILED` line at all, so a build break
# read as green. The status closes both.
#
# The needle is kept as a second, narrower signal and the output is captured so
# a failure can say WHAT failed rather than only that something did.
TEST_OUT="$(cargo test -p tui -p tui-render 2>&1)"; TEST_ST=$?
if [ "$TEST_ST" -ne 0 ] || grep -q "test result: FAILED" <<<"$TEST_OUT"; then
    bad "the restored crates do not pass their own tests (exit $TEST_ST)"
    grep -E "^(error|test result: FAILED)" <<<"$TEST_OUT" | head -3 | sed 's/^/        /'
else
    ok "cargo test -p tui -p tui-render is green"
fi
CLIPPY_OUT="$(cargo clippy -p tui -p tui-render --all-targets -- -D warnings 2>&1)"; CLIPPY_ST=$?
if [ "$CLIPPY_ST" -ne 0 ] || grep -qE '^(error|warning)' <<<"$CLIPPY_OUT"; then
    bad "the restored crates do not pass clippy -D warnings (exit $CLIPPY_ST)"
    grep -E '^(error|warning)' <<<"$CLIPPY_OUT" | head -3 | sed 's/^/        /'
else
    ok "clippy -D warnings is clean on both crates"
fi

head_ "result"
if [ "$FAILURES" -eq 0 ]; then
    printf '  \033[32mevery law proven discriminating, each after being forced to fail\033[0m\n\n'
    exit 0
fi
printf '  \033[31m%d check(s) failed\033[0m\n\n' "$FAILURES"
exit 1
