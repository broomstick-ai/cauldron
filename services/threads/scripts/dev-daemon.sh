#!/usr/bin/env bash
# dev-daemon.sh — THE ONE COMMAND that stands up a THROWAWAY `threads` daemon
# with dummy threads, so experiments stop happening against production.
#
# The operator: "You can maybe add an optional daemon path to do some testing on
# the daemon with dummy threads before messing with our prod."
#
#   lib/threads/scripts/dev-daemon.sh                 # seed ~/.threads-dev, serve :13653
#   lib/threads/scripts/dev-daemon.sh --scale 10      # ten times the synthetic data
#   lib/threads/scripts/dev-daemon.sh --seed-only     # seed and exit
#   lib/threads/scripts/dev-daemon.sh --no-seed       # serve what is already there
#   lib/threads/scripts/dev-daemon.sh --teardown      # delete ~/.threads-dev
#
# It is a THIN WRAPPER, exactly like `stress-test.sh`: it builds `threads-dev` if
# needed and hands every argument through. All the behaviour — the dev ports, the
# production guard, the environment isolation, the seeding, the banner — lives in
# the binary (`lib/threads/src/bin/threads-dev.rs`); run with `--help` for the
# flags. Nothing is duplicated here, because a wrapper that knew the ports would
# be a second place for them to be wrong.
#
# WHY A SEPARATE BINARY AND NOT `threads --dev`: see the module docs on
# `threads-dev.rs`. The short version is that with a flag, FORGETTING it is one
# keystroke from the operator's live port and live store; with a separate name,
# production is a different word.
#
# Exit codes are the binary's: 0 fine · 2 bad usage · 3 the production guard
# refused · 1 other error.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -P "$here/../../.." && pwd -P)"

bin="${CARGO_TARGET_DIR:-$repo/target}/debug/threads-dev"
if [[ ! -x "$bin" || -n "${DEV_DAEMON_REBUILD:-}" ]]; then
  # Build quietly to stderr so the banner is the first thing on stdout, then
  # prune whatever stale build artifacts THIS build superseded (operator ask,
  # msg #620 — see clean-build-scratch.sh; it deletes only hash-variants OLDER
  # than what this build just confirmed current, so it never forces a rebuild).
  "$here/clean-build-scratch.sh" -- \
    cargo build -q -p threads --bin threads-dev --manifest-path "$repo/Cargo.toml" >&2
fi

exec "$bin" "$@"
