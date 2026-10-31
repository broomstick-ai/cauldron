#!/usr/bin/env bash
# stress-test.sh — the one command the coordinator runs for a live budget or
# threshold stress test (round-7 item 8).
#
# It is a thin wrapper: it builds `threads-stress` if needed and hands every
# argument straight through. All the behavior — the exclusive lock, the refusal
# when anything else is running, the single execution, the outcome
# classification, the restore — lives in the binary
# (`lib/threads/src/bin/threads-stress.rs`); run with `--help` for the flags.
#
#   scripts/stress-test.sh --new-thread --cwd /tmp \
#     --threshold-fable 55 --text "hello"
#
#   scripts/stress-test.sh --thread <id> \
#     --key stress --accounts default --session-pct 0.5 --on-trip kill \
#     --text "count to five"
#
# Exit codes are the binary's: 0 ran · 2 bad usage · 3 REFUSED · 4 timed out.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -P "$here/../../.." && pwd -P)"

bin="$repo/target/debug/threads-stress"
if [[ ! -x "$bin" || -n "${STRESS_REBUILD:-}" ]]; then
  # Build quietly so stdout stays a clean JSON report, then prune whatever
  # stale build artifacts THIS build superseded (operator ask, msg #620: the
  # debug folders filling the disk) — see clean-build-scratch.sh for the rule.
  # Never forces a rebuild itself: it only deletes hash-variants OLDER than
  # what this build just confirmed current.
  "$here/clean-build-scratch.sh" -- \
    cargo build -q -p threads --bin threads-stress --manifest-path "$repo/Cargo.toml" >&2
fi

exec "$bin" "$@"
