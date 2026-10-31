#!/usr/bin/env bash
# test.sh — the canonical way to run this crate's test suite locally: `cargo
# test -p threads`, then prune whatever stale build artifacts that run
# superseded (operator ask, msg #620 — see clean-build-scratch.sh for the rule).
#
# The prune deletes only hash-variants OLDER than what this run just confirmed
# current, so it never forces a rebuild — but note that this holds ONLY because
# clean-build-scratch.sh now prunes the tree the build actually wrote
# (${CARGO_TARGET_DIR:-$repo/target}). It used to hardcode $repo/target, and under
# an exported CARGO_TARGET_DIR the age gate then protected nothing: the claim in
# this comment was false in exactly the case agents were told to work in, and it
# cost a signed deploy image on 7 October.
#
# Set CARGO_TARGET_DIR before running this. It is no longer merely advice: both
# halves refuse outright if the tree they would touch holds a signed deploy image.
#
#   scripts/test.sh                    # the whole suite
#   scripts/test.sh --lib budgets::    # any extra args pass through
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -P "$here/../../.." && pwd -P)"

"$here/clean-build-scratch.sh" -- \
  cargo test -p threads --manifest-path "$repo/Cargo.toml" "$@"
