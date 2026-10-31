#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  THE CPU BUDGET — idle and open CPU of the REAL BINARY against a daemon,
#  failing over budget. docs/THREAD-LOAD-LATENCY.md, F1-F8.
#
#  usage: cpu_budget_probe.sh <tui-binary> <thread-id> [idle-budget-pct] [port]
#
#  Runs the face headless twice on the same thread, for SHORT and LONG seconds,
#  and attributes the difference in CPU time to the LONG-SHORT seconds in which
#  the thread was open and settled. That steady-state percentage must be at or
#  under the budget (default 5%: the operator's bar is "idle near 0"; a thread
#  receiving live rows still has to stay well under one core).
#
#  Point it at the throwaway daemon (`threads-dev`, port 13653, seeded with
#  THREADS_DEV_BIG_EVENTS=20000 for a 4,000-row window) unless you mean to add
#  two read-only faces to the live one. It sends nothing either way: a headless
#  face has no input and calls no verb.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail
BIN=${1:?tui binary}
THREAD=${2:?thread id}
BUDGET=${3:-5}
[ -n "${4:-}" ] && export TUI_THREADS_CHASSIS_PORT=$4
SHORT=20
LONG=80

cpu_of() {
  TUI_DIAG=off /usr/bin/time -p "$BIN" --headless --thread "$THREAD" --seconds "$1" \
    --size 160x50 --out /dev/null 2>&1 >/dev/null | awk '/^(user|sys)/{t+=$2} END{print t}'
}

long=$(cpu_of $LONG)
short=$(cpu_of $SHORT)
pct=$(awk -v a="$long" -v b="$short" -v d=$((LONG - SHORT)) 'BEGIN{p=(a-b)/d*100; if(p<0)p=0; printf "%.2f", p}')
echo "cpu_s short(${SHORT}s)=$short long(${LONG}s)=$long steady_cpu_pct=$pct budget_pct=$BUDGET load=$(uptime | sed 's/.*averages: //')"
awk -v p="$pct" -v b="$BUDGET" 'BEGIN{exit !(p<=b)}' || { echo "FAIL: steady CPU ${pct}% over ${BUDGET}%"; exit 1; }
echo "PASS"
