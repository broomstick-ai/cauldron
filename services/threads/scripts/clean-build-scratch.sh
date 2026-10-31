#!/usr/bin/env bash
# clean-build-scratch.sh — post-run cleanup for the threads crate's test/stress
# tooling (operator ask, msg #620: "my disk is filling up a lot because of the
# debug folders in these directories ... make those folders get cleaned up
# after every test").
#
# Two things a stress/test run leaves behind:
#
#   1. STALE build artifacts. `cargo test`/`cargo build` never deletes a
#      superseded hash-variant of a binary it rebuilds — every recompile of a
#      test executable (or the lib itself) leaves its OLD hashed copy, plus
#      every incremental object file that went with it, sitting in the SHARED
#      workspace `target/`. Observed here: 3.5G in target/debug/deps and 1.4G
#      in target/debug/incremental, almost all of it stale copies of THIS
#      crate's own lib/bin/test artifacts (26k+ files matching just a handful
#      of name patterns).
#   2. SCRATCH data dirs a live run stood up (an ephemeral --data-dir, a
#      throwaway cwd) — separate from (1), just whatever path the run used.
#
# The rule for (1): touch a marker, run the wrapped command, then delete every
# entry belonging to THIS crate's own build targets (never another workspace
# member's) whose mtime is OLDER than the marker — i.e. NOT part of the
# fingerprint the just-finished build confirmed is current. This is NOT
# `cargo clean -p`: the one thing it can never delete is what the run just
# produced, so the NEXT build/test needs no recompilation. It is exactly the
# "stale incremental/test binaries" the operator described, pruned by age,
# scoped by name so nothing outside this crate is ever touched.
#
# Usage:
#   scripts/clean-build-scratch.sh -- cargo test -p threads
#   scripts/clean-build-scratch.sh --scratch /tmp/tcalib -- cargo build ...
#
# Prints one JSON line: the wrapped command's exit status, how many stale
# entries were pruned, and target/ size before and after (KB).
set -euo pipefail

here="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo="$(cd -P "$here/../../.." && pwd -P)"

# PRUNE THE TREE THE WRAPPED BUILD ACTUALLY WROTE, which is not necessarily
# "$repo/target".
#
# This line read `target="$repo/target"` and that is the defect behind the
# 7 October incident. The age gate below — `! -newer "$marker"`, which is this
# script's ENTIRE safety argument and is stated as fact in its header and in
# test.sh's — only protects current artifacts if the run it wrapped wrote into
# the same tree being pruned. Under an exported CARGO_TARGET_DIR it did not:
# cargo wrote to the scoped dir, so NOTHING in "$repo/target" was newer than the
# marker, every matching entry failed the age test, and a prune that advertised
# itself as "never forces a rebuild" became an unconditional wipe of the default
# tree's CURRENT artifacts for this crate.
#
# What that costs is not just a rebuild. `target/debug/threads` is a hard link to
# `target/debug/deps/threads-<hash>`; deleting the deps entry leaves the deploy
# path holding the signed bytes with a link count of 1 while `.fingerprint/`
# (which this prune does not touch) still says the bin is current. The tree is
# now primed so that the next cargo build in it relinks and re-uplifts over the
# deploy path — which is how an ad-hoc image came to sit where the supervisor
# launches from.
#
# Deriving the tree from the environment the wrapped command will inherit is the
# only form that cannot drift from it. Matches dev-daemon.sh:33.
target="${CARGO_TARGET_DIR:-$repo/target}"

# Every build-target name this crate itself produces (never another workspace
# member's) — the lib, its bin targets, and one per file under tests/. Kept as
# an explicit list, not a glob over Cargo.toml, so a rename elsewhere in the
# workspace can never widen what this script is allowed to delete.
names=(threads libthreads threads-stress)
for f in "$here"/../tests/*.rs; do
  [[ -e "$f" ]] || continue
  names+=("$(basename "$f" .rs)")
done

# `--scratch` takes a caller-supplied path and `rm -rf`s it, so it is the one
# argument here that could reach outside the build tree. It is allowed to name
# ONLY a throwaway directory: something under the temp dir, or under this
# workspace's own target/. Anything else — `$HOME`, `/`, a repo checkout, a
# captured account folder, `~/.threads` — is refused before the wrapped command
# even runs, so a mistyped invocation costs nothing.
scratch_ok() {
  local d="$1" parent tmp resolved
  [[ "$d" == /* ]] || return 1
  parent="$(cd "$(dirname "$d")" 2>/dev/null && pwd -P)" || return 1
  resolved="${parent%/}/$(basename "$d")"
  tmp="$(cd "${TMPDIR:-/tmp}" 2>/dev/null && pwd -P)" || tmp=/tmp
  case "$resolved" in
    "${tmp%/}"/?*) return 0 ;;
    /tmp/?* | /private/tmp/?* | /private/var/folders/?*) return 0 ;;
    "$target"/?*) return 0 ;;
  esac
  return 1
}

scratch_dirs=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --scratch)
      if [[ -z "${2:-}" ]] || ! scratch_ok "$2"; then
        echo "clean-build-scratch.sh: refusing --scratch ${2:-<empty>}: only a \
throwaway dir under ${TMPDIR:-/tmp} or $target may be deleted" >&2
        exit 2
      fi
      scratch_dirs+=("$2")
      shift 2
      ;;
    --)
      shift
      break
      ;;
    *)
      echo "clean-build-scratch.sh: unexpected argument before '--': $1" >&2
      exit 2
      ;;
  esac
done
if [[ $# -eq 0 ]]; then
  echo "usage: clean-build-scratch.sh [--scratch <dir>]... -- <command...>" >&2
  exit 2
fi

# NEVER TOUCH A TREE THAT HOLDS A SIGNED DEPLOY IMAGE — checked HERE, before the
# wrapped command runs, so a wrong invocation costs nothing (the same placement,
# and the same reason, as scratch_ok above).
#
# lib/threads/build.rs refuses to BUILD into such a tree, which closes every path
# that WRITES one. It cannot close this one: a prune is an `rm -rf`, no build
# script runs, and deleting `deps/threads-<hash>` is precisely what primes the
# tree to relink over the deploy path on somebody else's next build.
#
# The test is the designated requirement naming the deploy identifier — the exact
# property the daemon's absorb path and the supervisor's cold-start gate check,
# and one only the signing key can produce, so cargo cannot create it by accident.
# An ad-hoc product in a scratch tree is not matched and is pruned as always.
deploy_identifier="com.harness.threads"   # <- lib/threads/src/update.rs STABLE_BUNDLE_ID
for profile in debug release; do
  product="$target/$profile/threads"
  [[ -e "$product" ]] || continue
  if codesign -d -r- "$product" 2>&1 | grep -qF "identifier \"$deploy_identifier\""; then
    echo "clean-build-scratch.sh: REFUSING to run against $target — $product is a SIGNED \
DEPLOY IMAGE (its designated requirement names $deploy_identifier). Both halves of this script \
are unsafe there: a build relinks an ad-hoc image over the path the supervisor launches from, \
and the prune deletes the deps/ entry that path is hard-linked to. Point CARGO_TARGET_DIR at a \
scratch directory. To replace the deploy image, use: promote publish threads" >&2
    exit 3
  fi
done

marker="$(mktemp "${TMPDIR:-/tmp}/clean-build-scratch.XXXXXX")"
trap 'rm -f "$marker"' EXIT

before_kb=0
[[ -d "$target" ]] && before_kb=$(du -sk "$target" 2>/dev/null | cut -f1)

set +e
"$@"
status=$?
set -e

pruned=0
for profile in debug release; do
  for kind in deps incremental; do
    dir="$target/$profile/$kind"
    [[ -d "$dir" ]] || continue
    for name in "${names[@]}"; do
      while IFS= read -r -d '' entry; do
        rm -rf "$entry"
        pruned=$((pruned + 1))
      done < <(find "$dir" -maxdepth 1 -name "${name}-*" ! -newer "$marker" -print0 2>/dev/null)
    done
  done
done

for d in "${scratch_dirs[@]+"${scratch_dirs[@]}"}"; do
  [[ -n "$d" && -e "$d" ]] && rm -rf "$d"
done

after_kb=0
[[ -d "$target" ]] && after_kb=$(du -sk "$target" 2>/dev/null | cut -f1)

printf '{"status":%d,"pruned_entries":%d,"target_kb_before":%d,"target_kb_after":%d,"reclaimed_kb":%d}\n' \
  "$status" "$pruned" "$before_kb" "$after_kb" "$((before_kb - after_kb))"

exit "$status"
