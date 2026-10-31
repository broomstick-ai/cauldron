#!/usr/bin/env bash
# `vendor-chassis.sh` — **THE ONE WAY THE FOUR THINGS MOVE INTO A CONSUMER
# REPO**, and `docs/consumer-seam.md` §7.4 means "the one way" literally:
#
#   1. the crate            lib/shapewire/{src,tests,check-features.sh}
#   2. the reference        lib/shapewire/seam/{mod,link,held,view,commands,ledger}.rs
#   3. the transcripts      lib/shapewire/seam/conformance/*.jsonl
#   4. the inventory        lib/shapewire/seam/inventory.json (committed; copied)
#                      plus lib/shapewire/seam/inventory.rs   (GENERATED beside it,
#                           in the same run, and the half the CONSUMER'S COMPILER
#                           reads — see the copy loop for why it lives in the kit)
#
# plus the drift guard itself (`tools/seamdiff/src/engine.rs`), because a
# vendored reference with no guard beside it is a text file nobody checks.
#
# "There is no other way any of those change, and a `VENDORED` whose four
# hashes do not match the files is itself a red test."
#
#   ./vendor-chassis.sh --dest <consumer-repo>/libs/shapewire [--dry-run]
#
# ## No daemon, no network, no judgment
#
# `inventory.json` is a COMMITTED ARTIFACT of this repo, written by
# `tools/make-inventory.py --declared` (offline: it reads every shape's consts —
# contract, ceilings and the cell manifest — through the producer's own
# `shapes-manifest` bin; see its header). This script COPIES it and refuses if
# the producer's surface has moved since it was taken. So a vendor
# bump is offline and deterministic, and the file a consumer checks itself
# against is one a reviewer saw land in a diff rather than one a script
# regenerated during a copy.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
seam="$(cd "$here/.." && pwd)"
crate="$(cd "$seam/.." && pwd)"
repo="$(cd "$crate/../.." && pwd)"

dest=""
dry=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest) dest="$2"; shift 2 ;;
    --dry-run) dry=1; shift ;;
    *) echo "vendor-chassis.sh: unknown argument \`$1\`" >&2; exit 2 ;;
  esac
done
[[ -n "$dest" ]] || { echo "vendor-chassis.sh: --dest is required" >&2; exit 2; }

# ── the commit being vendored ──────────────────────────────────────────────
#
# Read with `git -C` against the SOURCE repo and nothing else, so this script
# never cares what directory it was invoked from.
commit="$(git -C "$repo" rev-parse HEAD)"
committed_at="$(git -C "$repo" show -s --format=%cI HEAD)"
short="${commit:0:7}"
if [[ -n "$(git -C "$repo" status --porcelain -- libs/shapewire)" ]]; then
  echo "vendor-chassis.sh: libs/shapewire has uncommitted changes." >&2
  echo "  A stamp that names a commit the files are not at is a stamp that lies," >&2
  echo "  and the whole point of the stamp is that a consumer can check out what" >&2
  echo "  it names. Commit first." >&2
  exit 1
fi

echo "vendor-chassis.sh"
echo "  source : $repo @ $commit"
echo "  dest   : $dest"

# ── 4. the inventory: COPIED, and verified against the commit ─────────────
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

inv="$seam/inventory.json"
[[ -f "$inv" ]] || {
  echo "vendor-chassis.sh: $inv does not exist." >&2
  echo "  §7.2 makes it part of the kit. Run tools/make-inventory.py (it needs a" >&2
  echo "  daemon on the chassis door for the shape contracts) and commit it." >&2
  exit 1
}
# ── IS THE INVENTORY STALE? And "stale" is about the SURFACE, not about HEAD ─
#
# The first version of this check compared `inventory.json`'s `source_commit`
# with HEAD and refused on any difference. That is wrong and it was wrong
# immediately: HEAD moves on every commit to this repo, including the commit
# that adds this script, so the check would demand a daemon and a regenerated
# inventory for a typo fix in a doc. An inventory is stale when the PRODUCER'S
# SURFACE has moved, and that is a question with a precise answer.
#
# Two halves, and neither needs a daemon:
#
#   1. the METHODS are scrapeable, so they are RE-SCRAPED and compared. A
#      re-pathing on the daemon side is caught here, at the vendor bump, which
#      is §7.2's whole claim.
#   2. the SHAPE CONTRACTS and CELL MANIFESTS are not scrapeable (trait consts
#      with defaults — see make-inventory.py, whose `--declared` READS them
#      instead), so the test is whether any file that could change them has a
#      commit since the inventory was taken.
inv_commit="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source_commit"])' "$inv")"

python3 - "$inv" "$crate/src/server.rs" "$repo/services/threads/src/server.rs" <<'PY' || exit 1
import json, re, sys
inv_path, chassis_src, ws_src = sys.argv[1:4]


def scrape(path, name):
    body = open(path).read()
    m = re.search(r"pub const " + name + r": &\[&str\] = &\[(.*?)\n\];", body, re.S)
    if not m:
        sys.exit(f"vendor-chassis.sh: {name} not found in {path}; it moved or was renamed")
    return sorted(set(re.findall(r'"([^"]+)"', m.group(1))))


inv = json.load(open(inv_path))
now = sorted(set(scrape(chassis_src, "CHASSIS_METHODS")) | set(scrape(ws_src, "WS_METHODS")))
was = sorted(set(inv["methods"]))
if now != was:
    added = [m for m in now if m not in was]
    gone = [m for m in was if m not in now]
    print("vendor-chassis.sh: the producer's METHOD surface has moved since", file=sys.stderr)
    print("  inventory.json was taken. Re-run tools/make-inventory.py and commit it.", file=sys.stderr)
    for m in added:
        print(f"    + {m}", file=sys.stderr)
    for m in gone:
        print(f"    - {m}", file=sys.stderr)
    sys.exit(1)
PY

shape_sources="services/threads/src/shapes libs/shapewire/src/hub.rs"
# **THE COMMIT MUST BE ONE THIS REPO HAS.** Added when the kit moved from
# substrate into cauldron (R3.S2.0.finish): the committed inventory was taken in
# substrate, so its `source_commit` is not an object here, `git log` below fails,
# `2>/dev/null || true` swallows the failure, `moved` is empty — and the
# staleness check PASSES while checking nothing. Refuse instead.
if ! git -C "$repo" cat-file -e "${inv_commit}^{commit}" 2>/dev/null; then
  echo "vendor-chassis.sh: inventory.json was taken at ${inv_commit:0:7}, which is not a" >&2
  echo "  commit in $repo — it was taken in another repository. Re-run" >&2
  echo "  tools/make-inventory.py --declared here and commit it." >&2
  exit 1
fi
# shellcheck disable=SC2086
moved="$(git -C "$repo" log --oneline "$inv_commit..HEAD" -- $shape_sources 2>/dev/null || true)"
if [[ -n "$moved" ]]; then
  echo "vendor-chassis.sh: a shape source has changed since inventory.json was taken" >&2
  echo "  at ${inv_commit:0:7}, and a shape's \`contract\` and cell manifest are read off" >&2
  echo "  the declaration. Re-run tools/make-inventory.py --declared and commit it." >&2
  echo "$moved" | sed 's/^/    /' >&2
  exit 1
fi
cp "$inv" "$tmp/inventory.json"
echo "  inventory: $(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(len(d["methods"]), "methods,", len(d["shapes"]), "shapes, read from", d["shapes_read_from"])
' "$inv")"

# ── the reference, with its stamp rewritten ────────────────────────────────
#
# `SHAPE_VERSION` is the ONE line of the reference this script edits, and
# `seamdiff --inventory` asserts it equals `inventory.json`'s `source_commit`.
# A face never edits it; the vendor bump does, which is what makes the version a
# face declares on every subscribe a commit somebody can check out rather than
# a number somebody typed.
#
# **IT IS STAMPED FROM THE INVENTORY'S COMMIT AND NOT FROM HEAD**, and the
# difference matters. The first version of this script stamped HEAD, and the
# very first real run produced `shapewire@8e9075c` in the reference beside a
# `source_commit` of `6445362` in the inventory — which made the inventory
# guard redden on a kit that was perfectly consistent. The two fields answer two
# different questions:
#
#   SHAPE_VERSION       which producer SURFACE do I believe in
#   VENDORED/source_commit  which crate BYTES am I linking
#
# Rule 2.1's `consumer.shape_version` is the first question — *"say what you
# were built against"* — and what a consumer is built against is the surface it
# subscribes to, which is the inventory. The bytes are stamped in `VENDORED`,
# where a hash per file says far more than a short hash in a const could. The
# staleness check above is what guarantees the surface has not moved between
# the two, so they are allowed to be different commits without either one
# lying.
# **SEVEN FILES NOW, NOT SIX.** `inventory.rs` is GENERATED beside
# `inventory.json` by `tools/make-inventory.py`, in the same run, and it is the
# half that makes the producer's surface a BUILD INPUT of the consumer rather than
# a fixture a test tool reads. It is vendored exactly like the other six — copied
# as text, hashed into `VENDORED` — and for the operator's stated reason: the kit
# is already the thing that carries "the same" by hash, so both faces necessarily
# compile against ONE surface at ONE commit. Per-consumer generation would
# reintroduce the drift the seam exists to kill.
#
# It has no fence and never will. A face that hand-edits it is compiling against
# its own opinion of what the producer serves, and `seamdiff --inventory` says so.
mkdir -p "$tmp/seam/conformance" "$tmp/seam/tools"
for f in mod.rs link.rs held.rs view.rs commands.rs ledger.rs inventory.rs; do
  if [[ ! -f "$seam/$f" ]]; then
    echo "vendor-chassis.sh: $seam/$f is missing." >&2
    echo "  inventory.rs is GENERATED: run tools/make-inventory.py (or" >&2
    echo "  tools/make-inventory.py --rust-only, which regenerates it from the" >&2
    echo "  committed inventory.json) and commit it." >&2
    exit 1
  fi
  cp "$seam/$f" "$tmp/seam/$f"
done
python3 - "$tmp/seam/mod.rs" "${inv_commit:0:7}" <<'PY'
import re, sys
p, short = sys.argv[1], sys.argv[2]
s = open(p).read()
new, n = re.subn(r'(pub const SHAPE_VERSION: &str = ")[^"]*(";)',
                 rf'\1shapewire@{short}\2', s)
if n != 1:
    sys.exit(f"vendor-chassis.sh: expected one SHAPE_VERSION in mod.rs, found {n}")
open(p, 'w').write(new)
PY

cp "$seam"/conformance/*.jsonl "$tmp/seam/conformance/" 2>/dev/null || {
  echo "vendor-chassis.sh: no conformance transcripts to vendor." >&2
  echo "  §7.3 makes them part of the kit; a kit without them proves the two" >&2
  echo "  copies are the same TEXT and never that they FOLD the same." >&2
  exit 1
}
cp "$here/seamdiff/src/engine.rs" "$tmp/seam/tools/seamdiff-engine.rs"
cp "$here/shapes-list.py" "$tmp/seam/tools/shapes-list.py"
cp "${BASH_SOURCE[0]}" "$tmp/seam/tools/vendor-chassis.sh"

# ── the crate ──────────────────────────────────────────────────────────────
mkdir -p "$tmp/src" "$tmp/tests"
cp -R "$crate/src/." "$tmp/src/"
cp -R "$crate/tests/." "$tmp/tests/"
cp "$crate/check-features.sh" "$tmp/check-features.sh"
cp "$tmp/inventory.json" "$tmp/seam/inventory.json"

# ── the stamp ──────────────────────────────────────────────────────────────
#
# Every vendored file hashed, so a hand-edit anywhere under the destination is a
# red test and not a thing somebody notices later. `Cargo.toml` is deliberately
# NOT hashed and NOT copied: its `{ workspace = true }` pins are written
# literally in the consumer repo and that is the one sanctioned divergence.
{
  echo "# libs/shapewire — VENDORED STAMP"
  echo "#"
  echo "# Written by libs/shapewire/seam/tools/vendor-chassis.sh in the source repo."
  echo "# Read back by libs/shapewire/tests/vendored_stamp.rs, which fails on any"
  echo "# disagreement. Cargo.toml is NOT stamped: its dependency pins are written"
  echo "# literally and are the one sanctioned divergence from the source."
  echo "source_repo cauldron"
  echo "source_path libs/shapewire"
  echo "source_commit $commit"
  echo "source_committed_at $committed_at"
  echo "vendored_at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "script_version 2"
  echo "# ── sha256 of every vendored file, relative to libs/shapewire/ ──"
  (cd "$tmp" && find . -type f ! -name VENDORED | sed 's|^\./||' | LC_ALL=C sort \
    | while read -r f; do echo "sha256 $(shasum -a 256 "$f" | cut -d' ' -f1) $f"; done)
} > "$tmp/VENDORED"

if [[ $dry -eq 1 ]]; then
  echo "  DRY RUN — staged in $tmp, nothing written"
  (cd "$tmp" && find . -type f | sed 's|^\./|    |' | LC_ALL=C sort)
  trap - EXIT
  echo "$tmp"
  exit 0
fi

mkdir -p "$dest"
rm -rf "$dest/src" "$dest/tests" "$dest/seam"
cp -R "$tmp/." "$dest/"
echo "  wrote  : $(cd "$dest" && find . -type f | wc -l | tr -d ' ') files"
echo "  stamp  : $dest/VENDORED  (script_version 2, commit $short)"
echo
echo "Next, in the consumer repo:"
echo "  * each face's src/seam/ is a COPY of libs/shapewire/seam/*.rs, checked by"
echo "    tests/seam_reference.rs, which include!s seam/tools/seamdiff-engine.rs"
echo "  * tests/seam_conformance.rs replays seam/conformance/*.jsonl through"
echo "    that face's own Held::fold"
echo "  * both go RED until the copies follow this reference, which is the"
echo "    correct direction of pressure (§7.1)"
