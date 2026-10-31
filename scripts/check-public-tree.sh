#!/usr/bin/env bash
# check-public-tree.sh [<ref>]   (default: HEAD)
#
# The public repository is cut from this one with `git archive <ref>` (one
# definition: scripts/build-public-archive), which
# leaves out every path `.gitattributes` marks `export-ignore`. This guard
# builds that archive and FAILS (exit 1) if any internal-only path below is in
# it, or if any required public path below is missing from it.
#
# The list of internal paths lives HERE, not in `.gitattributes`, on purpose:
# a check that read its expectations from the same file `git archive` obeys
# would pass when a line was deleted from it. The two lists are kept in step
# by this script: drop an `export-ignore` line and the path leaks and this
# fails; add a path here without its attribute and this fails too.
#
# Exit: 0 clean · 1 an internal path is in the archive, or a required public
# path is not · 2 the check could not measure anything (no archive, empty archive, or none of the paths below
# exists in <ref>, so nothing was checked).
set -euo pipefail

ref="${1:-HEAD}"

# Internal-only paths (exact archive paths). Keep in step with .gitattributes.
INTERNAL_PATHS='
R3.S2.0.hub-inspect.md
R3.S2.0.hub-report.md
BUILDING.md
docs/ACCOUNT-EMAIL-ID.md
docs/ASSUMPTIONS-LOG.md
docs/build-target-size.md
docs/cutover-2026-10-09.md
docs/GAP-AUDIT.md
docs/MESSAGING-ORCHESTRATION.md
docs/PENDING-PROMISES.md
docs/PROTOLEDGER-INTEGRATION.md
docs/runbook-r5s3-hosts-all.md
docs/SHIMS.md
docs/STRUCTURED-OUTPUT.md
docs/supervision-model.md
docs/THREAD-LEDGER.md
docs/THREAD-LOAD-LATENCY.md
docs/THREADS-DASHBOARD-REVIEW.md
docs/THREADS-DB-AUDIT.md
docs/TUI-SUBAGENTS.md
docs/SUBAGENT-WAKE.md
docs/SHAPEWIRE-LIST-DELTAS.md
docs/navigation.md
scripts/prune-target
scripts/test-prune-target
apps/tui/HANDOFF.md
scripts/personal-patterns.private
services/threads/docs/R1.S4-promote-provenance-gate.patch
services/threads/docs/account-model-changes.md
services/threads/docs/aui-data-contract.md
services/threads/docs/live-push-compat.md
services/threads/docs/live-push-daemon.md
services/threads/docs/managed-subagents.md
services/threads/docs/placement-new-account-fix.md
services/threads/docs/pressure-visibility-and-ledger.md
services/threads/docs/proto-ledger.md
services/threads/docs/spawn-config-dir-guard.md
services/threads/docs/tactical-rebase.md
services/threads/scripts/threads-app/SHAPES-TUI.md
services/threads/scripts/threads-app/LIVE-PUSH-TUI.md
libs/tui-render/PROVENANCE.md
libs/projects/PROVENANCE.md
libs/aui-wire/PROVENANCE.md
libs/term-shell/PROVENANCE.md
scripts/prove-warnings
scripts/prove-compile-gate
scripts/prove-supervisor
scripts/prove-wave1
scripts/prove-alarm-hosted
'
# Public paths the archive must contain. README.md shows the image, so an
# export-ignore that caught it would publish a broken README; README.md links
# CHANGELOG.md and ROADMAP.md, so the same holds for those.
REQUIRED_PATHS='
docs/images/cauldron.png
README.md
CHANGELOG.md
ROADMAP.md
LICENSE
'
# A path every archive of this repo must contain: the positive sentinel that
# proves the archive listing and the match below can find a path at all.
SENTINEL='Cargo.toml'

GREP=/usr/bin/grep
top="$(git rev-parse --show-toplevel)"
work="$(mktemp -d "${TMPDIR:-/tmp}/check-public-tree.XXXXXX")"
# bash 3.2 exits 0 from a `set -u` abort when an EXIT trap is set; any exit
# that is not a deliberate `finish` is reported as 2 (REL-4.S7h).
FINISHED=
finish() { FINISHED=1; exit "$1"; }
trap 'st=$?; rm -rf "$work"; if [ -z "$FINISHED" ]; then echo "check-public-tree: aborted (status $st); nothing measured" >&2; exit 2; fi' EXIT

sha="$(git -C "$top" rev-parse --verify "$ref^{commit}")"

# The artifact itself comes from the ONE shared definition (REL-4.S7h), the
# same one check-no-personal-data.sh scans.
"$top/scripts/build-public-archive" "$sha" "$work/archive.tar" > /dev/null
tar -tf "$work/archive.tar" > "$work/archive.txt"
git -C "$top" ls-tree -r --name-only "$sha" > "$work/tree.txt"

archived="$(wc -l < "$work/archive.txt" | tr -d ' ')"
if [ "$archived" -eq 0 ]; then
  echo "check-public-tree: the archive of $ref ($sha) is empty; nothing measured" >&2
  finish 2
fi
if ! $GREP -qFx -- "$SENTINEL" "$work/archive.txt"; then
  echo "check-public-tree: sentinel '$SENTINEL' not found in the archive of $ref; the listing cannot be trusted" >&2
  finish 2
fi

present=0
leaked=0
while IFS= read -r p; do
  [ -n "$p" ] || continue
  if $GREP -qFx -- "$p" "$work/tree.txt"; then
    present=$((present + 1))
    if $GREP -qFx -- "$p" "$work/archive.txt"; then
      echo "LEAK: $p is in the archive (no export-ignore for it in .gitattributes at $ref)"
      leaked=$((leaked + 1))
    fi
  fi
done <<< "$INTERNAL_PATHS"

missing=0
while IFS= read -r p; do
  [ -n "$p" ] || continue
  if ! $GREP -qFx -- "$p" "$work/archive.txt"; then
    echo "MISSING: $p is not in the archive of $ref (export-ignored, or absent from the tree)"
    missing=$((missing + 1))
  fi
done <<< "$REQUIRED_PATHS"

if [ "$present" -eq 0 ]; then
  echo "check-public-tree: none of the internal paths exists in $ref ($sha); nothing was checked" >&2
  finish 2
fi

echo "check-public-tree: ref=$ref sha=${sha:0:8} archived=$archived internal_in_tree=$present leaked=$leaked missing=$missing"
if [ "$leaked" -ne 0 ] || [ "$missing" -ne 0 ]; then
  finish 1
fi
finish 0
