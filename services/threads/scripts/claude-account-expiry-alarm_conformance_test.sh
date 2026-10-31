#!/usr/bin/env bash
# Conformance test for claude-account-expiry-alarm.
#
# A GREEN YOU HAVE NOT FORCED TO FAIL IS NOT EVIDENCE. Every case here is
# constructed so that the WRONG behaviour would be the easy one to ship:
#
#   1. a slot at 6 days MUST raise a ticket        (the alarm fires at all)
#   2. a slot at 8 days MUST NOT                   (it is not just always-on)
#   3. a second run MUST NOT duplicate the ticket  (daily != a ticket a day)
#   4. a second run MUST NOT write anything        (idempotent, not merely
#                                                   de-duplicated)
#   5. a DEAD slot MUST say "expired", and MUST NOT report negative days
#   6. a moved deadline MUST close the ticket      (he signed in)
#   7. expiring -> dead MUST escalate the SAME ticket, not file a second
#   8. the boundary at exactly 7.00 days MUST fire (the threshold is "at or
#      below", and an off-by-one here loses a whole day of warning)
#   9. an unreadable credential MUST NOT read as healthy
#
# Every case runs against a FIXTURE inventory in a throwaway workspace. No real
# credential is read, no real keychain item is touched, and the operator's real
# ticket board is never written to.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ALARM="$SCRIPT_DIR/claude-account-expiry-alarm"
HUMAN_ACTION="${EXPIRY_ALARM_HUMAN_ACTION:-$HOME/code/harness/core-plugins/core/tools/human-action}"

PASS=0; FAIL=0
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL+1)); }
check(){ if [ "$1" = "1" ]; then ok "$2"; else bad "$2"; fi; }

TMP="$(mktemp -d -t expiry-alarm-test)"
trap 'rm -rf "$TMP"' EXIT
WS="$TMP/repo/.mind/workspaces/test-ws"
mkdir -p "$WS/human-actions"
( cd "$TMP/repo" && git init -q . )

export EXPIRY_ALARM_TEST=1
export EXPIRY_ALARM_WORKSPACE="$WS"
export EXPIRY_ALARM_LOG="$TMP/alarm.log"
export EXPIRY_ALARM_HUMAN_ACTION="$HUMAN_ACTION"

# ── fixture builder ──────────────────────────────────────────────────────────
# Days are converted to an absolute epoch at fixture-build time, so the fixture
# means the same thing whenever it runs. `state` is what claude-accounts would
# have computed; the alarm consumes it rather than recomputing it.
mkfixture() {  # mkfixture <file> <label>:<days>[:<state>] ...
  local out="$1"; shift
  python3 - "$out" "$@" <<'PY'
import json, sys, time, datetime
out = sys.argv[1]
now = time.time()
slots = []
for spec in sys.argv[2:]:
    parts = spec.split(":")
    label, days = parts[0], float(parts[1])
    state = parts[2] if len(parts) > 2 else None
    if state in ("no-credential", "unreadable"):
        slots.append({"label": label, "config_dir": f"/tmp/{label}",
                      "keychain_service": f"Claude Code-credentials-{label[:8]}",
                      "days_left": None, "expires_at_ms": None,
                      "expires_at": None, "state": state})
        continue
    ms = int((now + days*86400) * 1000)
    if state is None:
        # A DELIBERATE LIE. Unless a case asks for a specific state, the fixture
        # claims every slot is healthy — including the ones that are days dead.
        # The alarm must reach its own verdict from `expires_at_ms`, so if it
        # ever went back to trusting this field every threshold case below turns
        # red at once. A fixture that agreed with the implementation would be
        # testing the implementation against itself.
        state = "healthy"
    slots.append({
        "label": label, "config_dir": f"/tmp/{label}",
        "keychain_service": f"Claude Code-credentials-{label[:8]}",
        "days_left": round(days, 2), "expires_at_ms": ms,
        "expires_at": datetime.datetime.fromtimestamp(ms/1000, datetime.timezone.utc)
                              .strftime("%Y-%m-%dT%H:%M:%SZ"),
        "state": state})
json.dump({"at": "fixture", "threshold_days": 7.0, "slots": slots,
           "orphan_stores": []}, open(out, "w"), indent=2)
PY
}

run() { EXPIRY_ALARM_INVENTORY="$1" "$ALARM" "${@:2}" >/dev/null 2>&1; }
tf()  { printf '%s/human-actions/claude-login-%s.md' "$WS" "$1"; }
fm()  { grep -m1 "^$2:" "$(tf "$1")" 2>/dev/null | sed "s/^$2: *//"; }
reset_ws() { rm -f "$WS"/human-actions/*.md 2>/dev/null; }

printf '\n  claude-account-expiry-alarm — conformance\n\n'

# ── 1. a slot at 6 days FIRES ────────────────────────────────────────────────
reset_ws
mkfixture "$TMP/f6.json" "sixday:6.0"
run "$TMP/f6.json"
[ -f "$(tf sixday)" ] && r=1 || r=0
check "$r" "6 days left -> a ticket is raised"
check "$([ "$(fm sixday status)" = "open" ] && echo 1 || echo 0)" \
      "   the ticket is open"
check "$([ "$(fm sixday slot_state)" = "expiring" ] && echo 1 || echo 0)" \
      "   it is recorded as expiring"
# It must name the ACCOUNT and the DATE. A generic "some login is expiring" is
# worthless when there are five slots.
grep -q 'sixday' "$(tf sixday)" && r=1 || r=0
check "$r" "   it names the specific account"
grep -qE '20[0-9]{2}' "$(tf sixday)" && r=1 || r=0
check "$r" "   it names a specific date"

# ── 2. a slot at 8 days STAYS QUIET ──────────────────────────────────────────
reset_ws
mkfixture "$TMP/f8.json" "eightday:8.0"
run "$TMP/f8.json"
[ -f "$(tf eightday)" ] && r=0 || r=1
check "$r" "8 days left -> NO ticket (the alarm is not simply always-on)"

# ── 8. the boundary: exactly 7.00 days FIRES ─────────────────────────────────
# "at or below seven days". An off-by-one here silently costs a day of warning.
reset_ws
mkfixture "$TMP/f7.json" "sevenday:7.0"
run "$TMP/f7.json"
[ -f "$(tf sevenday)" ] && r=1 || r=0
check "$r" "exactly 7.00 days -> fires (threshold is AT or below)"

reset_ws
mkfixture "$TMP/f701.json" "justover:7.01"
run "$TMP/f701.json"
[ -f "$(tf justover)" ] && r=0 || r=1
check "$r" "7.01 days -> quiet (the boundary is not smeared)"

# ── 3+4. a second run does not duplicate, and writes nothing ─────────────────
reset_ws
mkfixture "$TMP/f6b.json" "repeat:6.0"
run "$TMP/f6b.json"
before_sum="$(shasum -a 256 < "$(tf repeat)")"
before_n="$(ls -1 "$WS"/human-actions/*.md 2>/dev/null | wc -l | tr -d ' ')"
run "$TMP/f6b.json"
run "$TMP/f6b.json"
after_sum="$(shasum -a 256 < "$(tf repeat)")"
after_n="$(ls -1 "$WS"/human-actions/*.md 2>/dev/null | wc -l | tr -d ' ')"
check "$([ "$before_n" = "1" ] && [ "$after_n" = "1" ] && echo 1 || echo 0)" \
      "three runs -> still exactly one ticket (no duplicate per day)"
check "$([ "$before_sum" = "$after_sum" ] && echo 1 || echo 0)" \
      "   and the ticket file is byte-identical (idempotent, not just deduped)"

# ── 5. a DEAD slot reads as dead, never as negative days ─────────────────────
# The expiring slot is in the SAME fixture as the dead one, so the contrast
# below is measured against a ticket that actually exists right now rather than
# one left over from an earlier case.
reset_ws
mkfixture "$TMP/fdead.json" "deadslot:-0.56" "livingslot:6.0"
run "$TMP/fdead.json"
[ -f "$(tf deadslot)" ] && r=1 || r=0
check "$r" "an already-dead slot -> a ticket is raised"
check "$([ "$(fm deadslot slot_state)" = "dead" ] && echo 1 || echo 0)" \
      "   it is recorded as dead, not as expiring"
grep -qi 'EXPIRED' "$(tf deadslot)" && r=1 || r=0
check "$r" "   the message says it EXPIRED (a different sentence)"
# The whole point: it must not render the countdown for a lapsed slot.
if grep -qE '(-[0-9]+\.[0-9]+ days|expiring in -|-0\.56)' "$(tf deadslot)"; then r=0; else r=1; fi
check "$r" "   it does NOT report negative days anywhere in the ticket"
check "$([ "$(fm deadslot blocking)" = "true" ] && echo 1 || echo 0)" \
      "   dead is escalated harder than expiring (blocking: true)"
# and the contrast case, from the same run, so the above is not vacuously true
check "$([ "$(fm livingslot blocking)" = "false" ] && echo 1 || echo 0)" \
      "   an expiring slot in the SAME run is not blocking (the two differ)"

# ── 7. expiring -> dead escalates the SAME ticket ────────────────────────────
reset_ws
mkfixture "$TMP/fesc1.json" "escalate:3.0"
run "$TMP/fesc1.json"
n1="$(ls -1 "$WS"/human-actions/*.md | wc -l | tr -d ' ')"
s1="$(fm escalate slot_state)"
mkfixture "$TMP/fesc2.json" "escalate:-1.0"
run "$TMP/fesc2.json"
n2="$(ls -1 "$WS"/human-actions/*.md | wc -l | tr -d ' ')"
s2="$(fm escalate slot_state)"
check "$([ "$n1" = "1" ] && [ "$n2" = "1" ] && echo 1 || echo 0)" \
      "expiring -> dead: still ONE ticket, not two"
check "$([ "$s1" = "expiring" ] && [ "$s2" = "dead" ] && echo 1 || echo 0)" \
      "   the same ticket was escalated in place"
grep -q 'Update' "$(tf escalate)" && r=1 || r=0
check "$r" "   the escalation is appended, so the history is kept"

# ── 6. a moved deadline CLOSES the ticket ────────────────────────────────────
reset_ws
mkfixture "$TMP/fmove1.json" "moved:2.0"
run "$TMP/fmove1.json"
check "$([ "$(fm moved status)" = "open" ] && echo 1 || echo 0)" \
      "a slot at 2 days -> ticket open"
mkfixture "$TMP/fmove2.json" "moved:28.0"
run "$TMP/fmove2.json"
check "$([ "$(fm moved status)" = "resolved" ] && echo 1 || echo 0)" \
      "   deadline moves to 28 days (he signed in) -> ticket closes"
# The status must be attributed to the ALARM, never to him. His authorship has
# exactly one spelling, which is none.
check "$([ "$(fm moved status_by)" = "claude-account-expiry-alarm" ] && echo 1 || echo 0)" \
      "   and the close is attributed to the alarm, not to the operator"

# ── 9. an unreadable credential is not silently healthy ──────────────────────
reset_ws
mkfixture "$TMP/funread.json" "opaque:0:unreadable"
run "$TMP/funread.json"
[ -f "$(tf opaque)" ] && r=1 || r=0
check "$r" "a credential whose deadline cannot be read -> raised, not assumed fine"

# ── the real inventory is never touched in dry-run ───────────────────────────
reset_ws
mkfixture "$TMP/fdry.json" "dryslot:1.0"
run "$TMP/fdry.json" --dry-run
[ -f "$(tf dryslot)" ] && r=0 || r=1
check "$r" "--dry-run decides but writes no ticket"

# ── every ticket it wrote passes the upstream lint ───────────────────────────
reset_ws
mkfixture "$TMP/flint.json" "lintslot:5.0" "lintdead:-2.0"
run "$TMP/flint.json"
if "$HUMAN_ACTION" lint "$WS" >/dev/null 2>&1; then r=1; else r=0; fi
check "$r" "every ticket the alarm writes passes \`human-action lint\`"

# ── the threshold is genuinely the alarm's, and is genuinely configurable ────
# Same fixture, two thresholds. If the verdict came from the inventory rather
# than from this file's own comparison, both runs would agree — and this pair
# is the only thing that can tell those two worlds apart.
reset_ws
mkfixture "$TMP/fthr.json" "tunable:10.0"
run "$TMP/fthr.json"
[ -f "$(tf tunable)" ] && r=0 || r=1
check "$r" "10 days with a 7-day threshold -> quiet"
reset_ws
run "$TMP/fthr.json" --threshold 14
[ -f "$(tf tunable)" ] && r=1 || r=0
check "$r" "   the same fixture at --threshold 14 -> fires (the threshold is real)"

printf '\n  %d passed, %d failed\n\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
