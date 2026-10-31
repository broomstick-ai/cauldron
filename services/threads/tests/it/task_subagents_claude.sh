#!/usr/bin/env bash
# R9.TS2's scripted `claude` (only `task_subagents` points THREADS_CLAUDE_BIN
# here; `isolate` runs that module in a process of its own). The prompt names
# `script=<path>`: a file the TEST wrote, one directive per line —
#   WAIT <path>   block until <path> exists (60 s ceiling)
#   anything else is a stream-json line, written to stdout verbatim.
# Then it exits 0, which is the `claude` process ending.
for a in "$@"; do
  if [[ "$a" == "/usage" ]]; then echo "usage: n/a"; exit 0; fi
done
PROMPT="$(cat)"
[[ "$PROMPT" == *"script="* ]] || { echo "task_subagents_claude: no script= in prompt" >&2; exit 2; }
SCRIPT="${PROMPT#*script=}"
SCRIPT="${SCRIPT%%[[:space:]]*}"
while IFS= read -r line || [[ -n "$line" ]]; do
  if [[ "$line" == WAIT\ * ]]; then
    f="${line#WAIT }"
    for _ in $(seq 1 3000); do [[ -f "$f" ]] && break; sleep 0.02; done
  elif [[ -n "$line" ]]; then
    printf '%s\n' "$line"
  fi
done < "$SCRIPT"
exit 0
