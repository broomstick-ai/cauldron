#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# FORCE EVERY `threads/log` GUARD RED, ONCE — and refuse to believe a red that
# is really a compile error.
#
# A guard never seen red is a decoration. This applies each mutation named in a
# test's own docstring, runs the two suites, records WHICH tests went red and the
# red's actual text, and reverts. It also asserts the OPPOSITION the suite claims:
# a mutation that reddens nothing is a guard that is not guarding, and a mutation
# that reddens EVERYTHING is a mutation that broke the fixture rather than the
# behaviour.
#
# Usage:  bash lib/threads/scripts/force-red-live-log.sh [out-dir]
#
# It edits tracked files in place and restores them with `git checkout --` at the
# end of every mutation, including on an interrupt. It never commits, never
# stashes (the stash stack is shared across worktrees) and never touches
# `lib/threads/threads.db`.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

ROOT="$(cd -P "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
OUT="${1:-${TMPDIR:-/tmp}/force-red-live-log}"
mkdir -p "$OUT"
cd "$ROOT"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/.target}"
export CARGO_PROFILE_TEST_DEBUG=0

FOLD=libs/shapewire/src/fold.rs
HUB=libs/shapewire/src/hub.rs
WIRE=libs/shapewire/src/wire.rs
LOG=services/threads/src/shapes/log.rs
BODY=services/threads/src/shapes/body.rs
CHASSIS=services/threads/src/chassis.rs
SIZE=services/threads/tests/live_log_size.rs
TOUCHED=("$FOLD" "$HUB" "$WIRE" "$LOG" "$BODY" "$CHASSIS" "$SIZE")

restore() { git checkout -- "${TOUCHED[@]}" 2>/dev/null || true; }
trap restore EXIT INT TERM

# `python3 - <<'EOF'` with an exact-match assertion, so a mutation whose anchor
# has moved is a loud failure of THIS script and never a silently-skipped one.
patch() { # patch <file> <old> <new>
  python3 - "$1" "$2" "$3" <<'PY'
import sys
p, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p).read()
n = s.count(old)
if n != 1:
    sys.exit(f"ANCHOR NOT UNIQUE ({n}) in {p}: {old[:90]!r}")
open(p, "w").write(s.replace(old, new))
PY
}

pass=0; bad=0

run_mutation() { # run_mutation <name> <expect-red-regex>
  local name="$1" expect="$2" log="$OUT/$1.log"
  cargo test -p threads --test live_log --test live_log_size --no-fail-fast \
    -- --test-threads=1 >"$log" 2>&1
  if grep -q 'error\[E' "$log"; then
    printf '  NOT A RED  %-56s compile error — the mutation did not typecheck\n' "$name"
    grep -m2 'error\[E' "$log" | sed 's/^/             /'
    bad=$((bad+1)); return
  fi
  local reds
  reds="$(grep -E '^test .* FAILED$' "$log" | sed -e 's/^test //' -e 's/ \.\.\. FAILED$//' | sort)"
  if [ -z "$reds" ]; then
    printf '  NO RED     %-56s nothing went red: the guard is a decoration\n' "$name"
    bad=$((bad+1)); return
  fi
  if ! printf '%s\n' "$reds" | grep -qE "$expect"; then
    printf '  WRONG RED  %-56s expected /%s/, got:\n' "$name" "$expect"
    printf '%s\n' "$reds" | sed 's/^/             /'
    bad=$((bad+1)); return
  fi
  printf '  RED        %-56s %s\n' "$name" "$(printf '%s' "$reds" | tr '\n' ' ')"
  # The red's ACTUAL TEXT, which is what the docstrings quote.
  {
    echo "### $name"
    echo "$reds"
    echo '---'
    awk '/^---- /{p=1} /^failures:$/{p=0} p' "$log" | head -40
  } >>"$OUT/reds.md"
  pass=$((pass+1))
}

: >"$OUT/reds.md"
echo "forcing every guard red, one at a time — $(git rev-parse --short HEAD)"

# ── 1. the hydrate, both directions (the opposing pair) ──────────────────────
patch "$LOG" '        if events.is_empty() {' '        if true {'
run_mutation 'hydrate-always-refuses' 'a_cold_subscriber|a_resubscribed_mirror|the_hydrate_and_the_live'
restore

patch "$LOG" '        if events.is_empty() {
' '        root.rows.key(id.clone());
        if events.is_empty() {
'
run_mutation 'hydrate-always-mints' 'a_thread_with_no_log_produces_no_row_at_all'
restore

# ── 2. the headline: ONE DROPPED APPEND ─────────────────────────────────────
patch "$LOG" '                if held.is_some_and(|h| se.seq <= h) {
                    return;
                }' '                if held.is_some_and(|h| se.seq <= h) {
                    return;
                }
                if se.seq % 3 == 0 {
                    return;
                }'
run_mutation 'one-dropped-append' 'a_resubscribed_mirror_is_identical_to_one_that_never_disconnected'
restore

# ── 3. the fold's two directions (the opposing pair) ────────────────────────
patch "$LOG" '                if held.is_some_and(|h| se.seq <= h) {' '                if held.is_some_and(|h| se.seq > h) {'
run_mutation 'fold-appends-nothing' 'a_resubscribed_mirror|the_bulk_kinds|the_hydrate_and_the_live'
restore

patch "$LOG" '                if held.is_some_and(|h| se.seq <= h) {
                    return;
                }
' ''
run_mutation 'fold-appends-every-replay' 'a_replayed_seq_is_not_appended_twice'
restore

# ── 4. the bulk kinds, both directions ──────────────────────────────────────
patch "$LOG" '            Input::Push(Push::Event(se)) => {
                if !holds(tx, &se.thread_id) {' '            Input::Push(Push::Event(se)) => {
                if crate::shapes::threads::BULK_KINDS.contains(&se.event.kind()) {
                    return;
                }
                if !holds(tx, &se.thread_id) {'
run_mutation 'bulk-kinds-discarded' 'the_bulk_kinds_are_in_the_window'
restore

patch "$LOG" '                if !holds(tx, &se.thread_id) {
                    return;
                }' '                if false {
                    return;
                }'
run_mutation 'scope-guard-deleted' 'the_fold_mints_no_key_for_a_thread_nobody_opened'
restore

# ── 5. the key ceiling, both directions ─────────────────────────────────────
patch "$LOG" '    const MAX_OPEN_KEYS_PER_CONN: Option<usize> = Some(OPEN_WINDOWS);' '    const MAX_OPEN_KEYS_PER_CONN: Option<usize> = None;'
run_mutation 'key-ceiling-never-fires' 'the_ninth_open_window_on_one_connection'
restore

patch "$HUB" '            if !set.contains(k) && set.len() >= max {' '            if set.len() >= max {'
run_mutation 'key-ceiling-counts-resubscribes' 'a_resubscribe_to_an_open_key_costs_no_slot'
restore

# ── 6. the retain ceiling, refused vs clamped ───────────────────────────────
patch "$HUB" '            if n > slot.max.retained_items {
                return Err(ShapeError::AboveMax {' '            if false {
                return Err(ShapeError::AboveMax {'
run_mutation 'retain-ceiling-not-refused' 'a_retain_above_the_window_is_refused_naming_the_ceiling'
restore

patch "$LOG" '    fn row_at(&self, key: &str, retain: shapewire::wire::Retain) -> Option<Value> {
        self.rows
            .get(&key.to_string())
            .map(|r| r.to_json(key, retain))' '    fn row_at(&self, key: &str, retain: shapewire::wire::Retain) -> Option<Value> {
        let _ = retain;
        self.rows
            .get(&key.to_string())
            .map(|r| r.to_json(key, shapewire::wire::Retain::Max))'
run_mutation 'retain-accepted-then-ignored' 'a_retain_under_the_ceiling_shows_the_newest_n_on_every_frame'
restore

# ── 7. where the bytes go, both directions ──────────────────────────────────
patch "$LOG" 'pub const MAX_INLINE_PAYLOAD: usize = 1_024;' 'pub const MAX_INLINE_PAYLOAD: usize = usize::MAX;'
run_mutation 'nothing-ever-leaves-the-frame' 'a_large_message_splits|a_bodiless_event_past_the_bound'
restore

patch "$LOG" 'pub const MAX_INLINE_PAYLOAD: usize = 1_024;' 'pub const MAX_INLINE_PAYLOAD: usize = 0;'
run_mutation 'everything-leaves-the-frame' 'a_small_event_rides_the_frame_whole|a_delta_costs_less'
restore

# ── 8. the summary's bytes vs what the door serves ──────────────────────────
patch "$BODY" '        served.len() as u64,' '        served.len() as u64 + 1,'
run_mutation 'summary-bytes-disagree-with-the-door' 'a_summarys_bytes|a_large_message_splits'
restore

# ── 9. the chassis: the item spelling, and the window's own statement ───────
patch "$FOLD" '        self.pending.push(stamped_item(&item, at));' '        self.pending
            .push(serde_json::to_value(&item).unwrap_or(Value::Null));'
run_mutation 'transition-item-loses-its-stamp' 'a_resubscribed_mirror|the_hydrate_and_the_live'
restore

patch "$FOLD" '            complete: complete && skip == 0,' '            complete,'
run_mutation 'a-truncated-window-claims-to-be-the-log' 'past_the_capacity|a_thread_with_a_hundred'
restore

patch "$FOLD" '        let skip = entries.len().saturating_sub(self.capacity);' '        let skip = 0;'
run_mutation 'read-ignores-the-capacity' 'a_thread_with_a_hundred|past_the_capacity'
restore

# The anchor is the COMMENT, which is unique to `Sequence::append` — the flag-carry
# block itself appears in three cells and `patch` refused it (correctly, loudly).
patch "$FOLD" '        let (was_complete, was_hydrated) = match &self.state {
            CellState::Known {
                complete,
                since_init,
                ..
            } => (*complete, !*since_init),
            _ => (true, false),
        };
        self.state = CellState::Known {
            complete: was_complete,
            folded_at: at,
            since_init: !was_hydrated,
            covers_since: self.items.front().map(|i| i.at),
        };
        true
    }' '        self.state = CellState::Known {
            complete: true,
            folded_at: at,
            since_init: true,
            covers_since: self.items.front().map(|i| i.at),
        };
        true
    }'
run_mutation 'an-append-erases-the-hydrate' 'past_the_capacity|a_cold_subscriber'
restore

patch "$LOG" '            events: Sequence::new_capacity_only(WINDOW),' '            events: Sequence::new(WINDOW, chrono::Duration::hours(24)),'
# **THE FIRST PASS CAME BACK `NO RED` HERE, AND THE TEST THAT CATCHES IT WAS
# WRITTEN BECAUSE OF THAT.** The only guard was
# `a_count_bounded_window_does_not_empty_itself_overnight`, a UNIT test over
# `Sequence::new_capacity_only` — which this mutation does not touch, because it
# mutates the SHAPE's choice of cell and not the cell. A guard that cannot see the
# one mutation it exists for is a decoration, so
# `the_shapes_window_has_no_clock_so_a_week_old_transcript_survives_an_append`
# drives the shape.
run_mutation 'the-window-grows-a-clock' 'the_shapes_window_has_no_clock'
restore

# ── 10. the measurement itself ─────────────────────────────────────────────
patch "$SIZE" 'const BUDGET: usize = 1_024 * 1_024;' 'const BUDGET: usize = 1;'
run_mutation 'the-budget-is-a-real-bar' 'an_unfiltered_window_of_four_thousand_rows_fits_its_budget'
restore

# ── 11. no activity claim comes back ───────────────────────────────────────
# **ALSO `NO RED` ON THE FIRST PASS**, and for a different reason worth keeping: the
# mutation added a key INSIDE `limits`, which is one cell's opaque value and not a
# shape-level cell. The guard asserts the `shared` BLOCK's key set, which is where an
# activity claim would actually have to live, so the mutation has to put it there.
patch "$LOG" '        json!({
            "limits": self.limits.now(),
            "shape_version": self.shape_version.now(),
        })' '        json!({
            "last_event_at": "any event is activity, which is the defect 37187d6 fixed",
            "limits": self.limits.now(),
            "shape_version": self.shape_version.now(),
        })'
run_mutation 'a-shape-level-activity-claim' 'the_log_makes_no_activity_claim_and_has_nowhere_to_put_one'
restore

# ── 12. the door's widening ────────────────────────────────────────────────
patch "$BODY" '    if let Some(t) = body_text(event) {
        return (t.to_string(), BODY_CONTENT_TYPE);
    }' '    if let Some(t) = body_text(event) {
        return (t.to_string(), PAYLOAD_CONTENT_TYPE);
    }'
run_mutation 'a-message-body-changes-media-type' 'a_summarys_bytes_is_what_the_door_will_deliver'
restore

echo
echo "forced red: $pass    unbelievable: $bad"
echo "the reds' own text: $OUT/reds.md"
[ "$bad" -eq 0 ] || exit 1
