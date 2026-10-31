#!/usr/bin/env bash
# ── EVERY FEATURE COMBINATION COMPILES, AND THE FEATURE GRAPH SAYS WHAT THE
#    MANIFEST CLAIMS IT SAYS ───────────────────────────────────────────────────
#
# Two halves, and the SECOND one is why this file exists in cauldron rather than
# being carried over from substrate's:
#
#   PART 1 — THE GRAPH. `libs/shapewire/Cargo.toml` makes three claims about
#   WHO CAN REACH WHAT, and all three used to be comments that held by luck:
#     * the floor (`--no-default-features`) is what the two faces VENDOR, and it
#       must pull neither tokio nor axum nor cauldron's `env`;
#     * `--features server` is the network DOOR and must not pull `env` either
#       — `seam` is a separate axis precisely so this is a fact about the graph
#       and not a promise about today's callers;
#     * `anyhow`, `toml` and `tracing-subscriber` are not DIRECT dependencies of
#       this crate at any flag (a subscriber is the binary's choice; errors that
#       cross the wire are local types; decision 2 needs no file format).
#   These are resolved from the dependency graph, not grepped out of a built
#   artifact: `opt-level=3` can delete a string but it cannot delete an edge.
#
#   PART 2 — THE COMBINATIONS. substrate's `docs/shapes.md` 6.4 predicted this
#   rot and then it happened: four of eight flag combinations did not compile,
#   because `hub` used `crate::ledger` unconditionally while `lib.rs` gated that
#   module, and nothing built the broken seven. `hub` genuinely requires `fold`
#   and `ledger`, and `server` genuinely requires `hub` and `command`, so the
#   free axes are `fold`, `ledger`, `command`, plus the two that imply others.
#
# ── TWO LESSONS CARRIED, EACH PAID FOR ELSEWHERE ─────────────────────────────
#
# 1. THE CRATE DIR IS RESOLVED ABSOLUTELY BEFORE ANYTHING MOVES. `$0` is
#    relative when the script is invoked relatively, so a `dirname "$0"` read
#    AFTER a `cd` is a different path — in substrate that pointed a probe
#    crate's `path =` dependency at a virtual manifest, cargo's refusal was
#    swallowed by a `>/dev/null`, and the script reported two builds BROKEN that
#    were fine (2026-10-05, a whole debugging detour).
#
# 2. EVERY "FINDS NOTHING" ASSERTION IS PAIRED WITH A "FINDS SOMETHING" OVER THE
#    SAME PREDICATE, THE SAME SCOPE AND THE SAME BOUNDS. Writing this file, a
#    first pass looped over four flag sets held in a shell variable; under zsh an
#    unquoted expansion is NOT word-split, so three of the four reached cargo as
#    ONE argument, cargo refused, stderr was discarded, and the run reported
#    `axum: 0` for `--features server` — a dead search that reads exactly like a
#    clean one. Hence `bash` with explicit arrays below, `rc` captured off every
#    cargo invocation, and `want_absent` never standing alone.
#
# Run it SERIALLY AND ALONE: a concurrent cargo holding the package-cache lock
# makes this report false failures.
#
# `CARGO_TARGET_DIR` IS REQUIRED, AND THERE IS NO DEFAULT (HOLDS R13). This
# used to fall back to `$REPO/.target`: a target INSIDE the repository, which
# the arc's standing rule forbids by name (a fixture placed inside a repository
# measured 1274/1 against 1275/0 out of it, substrate `-p threads`), and which on
# this machine silently costs a private multi-GiB build of everything. A caller
# who names nothing is refused before cargo runs, with the fix in the message.
set -uo pipefail

CRATE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$CRATE/../.." && pwd)"
if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf 'check-features.sh: REFUSED: CARGO_TARGET_DIR is unset. Name the release'"'"'s shared\n' >&2
    printf '  target directory, outside every repository, e.g.\n' >&2
    printf '  CARGO_TARGET_DIR=~/code/ct-cauldron-<release> %s\n' "$0" >&2
    printf '  (no default: the old one was %s/.target, inside the repository)\n' "$REPO" >&2
    exit 2
fi
case "$CARGO_TARGET_DIR" in
    "$REPO"|"$REPO"/*)
        printf 'check-features.sh: REFUSED: CARGO_TARGET_DIR=%s is inside the repository %s.\n' \
            "$CARGO_TARGET_DIR" "$REPO" >&2
        exit 2 ;;
esac
export CARGO_TARGET_DIR
export CARGO_INCREMENTAL=0

PASS=0
FAIL=0
head2() { printf '\n── %s %s\n' "$1" "$(printf '─%.0s' $(seq 1 $((70 - ${#1}))))"; }
ok()   { PASS=$((PASS + 1)); printf '  ok    %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); printf '  FAIL  %s\n     expected: %s\n     actual:   %s\n' "$1" "$2" "$3"; }
eq()   { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "$2" "$3"; fi; }

# The resolved, transitive, NORMAL-dependency package names for one flag set.
# `-e normal` drops dev- and build-dependencies, which is the point: the floor's
# own dev-dependencies DO pull tokio, and a face vendoring `src/` never builds
# them. `rc` is captured directly, so a cargo that refuses the flags is a
# failure here and not a silent empty set.
tree_of() {
    local out rc
    out="$(cargo tree --manifest-path "$CRATE/Cargo.toml" -p shapewire -e normal --prefix none "$@" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'CARGO-REFUSED rc=%s\n%s\n' "$rc" "$out"
        return 1
    fi
    printf '%s\n' "$out" | sed 's/ (\*)$//' | awk 'NF {print $1}' | sort -u
}

# `cargo tree --depth 1` lists only what the manifest itself names.
direct_of() {
    local out rc
    out="$(cargo tree --manifest-path "$CRATE/Cargo.toml" -p shapewire -e normal --prefix none --depth 1 "$@" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'CARGO-REFUSED rc=%s\n%s\n' "$rc" "$out"
        return 1
    fi
    printf '%s\n' "$out" | sed 's/ (\*)$//' | awk 'NF {print $1}' | sort -u
}

has() { printf '%s\n' "$1" | grep -qxF "$2" && echo yes || echo no; }

# ═════════════════════════════════════════════════════════════════════════════
head2 "PART 1 — THE FEATURE GRAPH"

FLOOR="$(tree_of --no-default-features)"       || { echo "$FLOOR"; exit 2; }
DEFAULT="$(tree_of)"                           || { echo "$DEFAULT"; exit 2; }
SERVER="$(tree_of --no-default-features --features server)" || { echo "$SERVER"; exit 2; }
SEAM="$(tree_of --no-default-features --features seam)"     || { echo "$SEAM"; exit 2; }
ALL="$(tree_of --all-features)"                || { echo "$ALL"; exit 2; }

# ── THE FLOOR: what a face vendors ──────────────────────────────────────────
# Each absence is followed by the presence that proves the same lookup, over the
# same set, can answer yes — so "no tokio in the floor" cannot pass because the
# set was empty or the name was misspelt.
eq "the floor is non-empty at all (it names itself)" yes "$(has "$FLOOR" shapewire)"
eq "the floor does NOT pull tokio"                   no  "$(has "$FLOOR" tokio)"
eq "the floor does NOT pull axum"                    no  "$(has "$FLOOR" axum)"
eq "the floor does NOT pull env"                     no  "$(has "$FLOOR" env)"
eq "  ...and the floor DOES pull serde_json, by the same lookup" \
                                                     yes "$(has "$FLOOR" serde_json)"
eq "  ...and the floor DOES pull tracing, by the same lookup" \
                                                     yes "$(has "$FLOOR" tracing)"

# ── THE DOOR IS NOT THE SEAM: the amendment, as a property ──────────────────
eq "--features server pulls axum"                    yes "$(has "$SERVER" axum)"
eq "--features server pulls tokio (via hub)"         yes "$(has "$SERVER" tokio)"
eq "--features server pulls uuid (the bearer)"       yes "$(has "$SERVER" uuid)"
eq "--features server pulls sha2 (the resource digest)" yes "$(has "$SERVER" sha2)"
eq "--features server does NOT pull env"             no  "$(has "$SERVER" env)"
eq "--features seam DOES pull env — the SAME lookup, answering yes" \
                                                     yes "$(has "$SEAM" env)"
eq "--features seam pulls axum too (seam requires server)" \
                                                     yes "$(has "$SEAM" axum)"
eq "--all-features pulls env"                        yes "$(has "$ALL" env)"
eq "the default set does NOT pull axum"              no  "$(has "$DEFAULT" axum)"
eq "the default set DOES pull tokio (hub is default)" yes "$(has "$DEFAULT" tokio)"

# ── WHAT THIS CRATE REFUSES TO DEPEND ON DIRECTLY ───────────────────────────
# Transitively, `env` brings `anyhow` and `toml` in behind `seam`, and that is
# not what is being forbidden: the claim is that SHAPEWIRE's own manifest names
# neither, so nothing on the floor or behind `server` can reach them and the
# types that cross the wire stay local. Hence `--depth 1`, which is the scope
# the claim is actually about.
DIRECT_ALL="$(direct_of --all-features)" || { echo "$DIRECT_ALL"; exit 2; }
eq "direct deps at --all-features are non-empty"     yes "$(has "$DIRECT_ALL" serde)"
eq "shapewire does not DIRECTLY depend on anyhow"    no  "$(has "$DIRECT_ALL" anyhow)"
eq "shapewire does not DIRECTLY depend on toml"      no  "$(has "$DIRECT_ALL" toml)"
eq "shapewire does not DIRECTLY depend on tracing-subscriber" \
                                                     no  "$(has "$DIRECT_ALL" tracing-subscriber)"
eq "  ...and anyhow IS reachable transitively behind seam, via env — so the" \
                                                     yes "$(has "$SEAM" anyhow)"
eq "  ...two scopes above are genuinely different scopes" \
                                                     yes "$(has "$ALL" toml)"
eq "shapewire DOES directly depend on env at --all-features" \
                                                     yes "$(has "$DIRECT_ALL" env)"

# ═════════════════════════════════════════════════════════════════════════════
head2 "PART 2 — EVERY COMBINATION COMPILES"

# All eight subsets of the three free flags, then the two that imply others, then
# the cauldron pair, then everything. Nothing below is a combination nobody
# builds.
COMBOS=(
  ""
  "fold"
  "ledger"
  "command"
  "fold,ledger"
  "fold,command"
  "ledger,command"
  "fold,ledger,command"
  "hub"
  "hub,command"
  "hub,ledger,command"
  "server"
  "seam"
)
for c in "${COMBOS[@]}"; do
    if [ -z "$c" ]; then
        cargo check --manifest-path "$CRATE/Cargo.toml" -p shapewire \
            --no-default-features > /dev/null 2>&1
        rc=$?
        eq "cargo check --no-default-features                 (the floor)" 0 "$rc"
    else
        cargo check --manifest-path "$CRATE/Cargo.toml" -p shapewire \
            --no-default-features --features "$c" > /dev/null 2>&1
        rc=$?
        eq "cargo check --no-default-features --features $(printf '%-22s' "$c")" 0 "$rc"
    fi
done
cargo check --manifest-path "$CRATE/Cargo.toml" -p shapewire --all-features > /dev/null 2>&1
eq "cargo check --all-features" 0 "$?"
cargo check --manifest-path "$CRATE/Cargo.toml" -p shapewire > /dev/null 2>&1
eq "cargo check (the default set, named as such)" 0 "$?"

head2 "RESULT"
printf '%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
